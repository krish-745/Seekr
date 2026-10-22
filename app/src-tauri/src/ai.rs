//! AI answers. `?question` asks the model on this PC about the user's files; `??question` asks
//! a cloud model anything, sending only the question (never a file).
//!
//! `?`:  The passages that match best
//! (by words and meaning, about 3,000 tokens) go to the model numbered, the answer streams into
//! the preview pane citing them, and follow-up questions keep the earlier passages first so the
//! model's prompt cache still holds them.
//!
//! The model is Qwen3.5-4B in llama.cpp's server on the GPU by default (see
//! docs/benchmarks/llm.md), started on the first question and stopped after ten idle minutes,
//! which hands the GPU back.
//!
//! `??`: Gemini or Grok, through their OpenAI-style APIs, with the key in the user's environment
//! (GEMINI_API_KEY or XAI_API_KEY). `api` in config.toml picks one when both are set, and
//! `api_model` the model.

use std::{
    io::{BufRead, BufReader},
    net::TcpListener,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager, ipc::Channel};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW},
            Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
        },
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::{
    content::{self, Passage},
    query::{Action, Item},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// About 3,000 tokens of passages for a question, and 1,500 more for each follow-up, within
/// the 8,192-token context.
const FIRST_BUDGET: usize = 12_000;
const FOLLOW_UP_BUDGET: usize = 6_000;
const CONTEXT_TOKENS: &str = "8192";
const MAX_ANSWER_TOKENS: u32 = 600;
const IDLE: Duration = Duration::from_secs(10 * 60);
const RUNTIME: &str = r"llm\llama.cpp-b11819\llama-server.exe";
const MODELS: &str = r"llm\models";
const DEFAULT_MODEL: &str = "Qwen3.5-4B-Q4_K_M";

/// A cloud API that speaks OpenAI's chat format.
#[derive(Debug)]
struct Api {
    /// Its name for `ai =` in config.toml.
    name: &'static str,
    label: &'static str,
    url: &'static str,
    /// The environment variable holding the key.
    key: &'static str,
    model: &'static str,
    /// A request field that turns thinking off, for a faster first word; dropped if refused.
    no_thinking: Option<(&'static str, &'static str)>,
}

const APIS: [Api; 2] = [
    Api {
        name: "gemini",
        label: "Gemini API",
        url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
        key: "GEMINI_API_KEY",
        model: "gemini-2.5-flash",
        no_thinking: Some(("reasoning_effort", "none")),
    },
    Api {
        name: "grok",
        label: "xAI Grok API",
        url: "https://api.x.ai/v1/chat/completions",
        key: "XAI_API_KEY",
        model: "grok-4-fast-non-reasoning",
        no_thinking: None,
    },
];

const SYSTEM: &str = "You answer questions about the user's own files. Use only the numbered \
    passages. Be brief and direct. Cite the passages you used after the sentence they support, \
    like [2]. If the passages don't answer the question, say you couldn't find it in their files.";

const GENERAL: &str = "You are a helpful assistant inside a Windows launcher. Answer directly \
    and briefly; use a short list or code block only when it helps.";

const ASK: Action = Action {
    id: "ask",
    label: "Ask",
};

struct Server {
    child: Child,
    /// Ends the server with Seekr, however Seekr ends.
    job: HANDLE,
    port: u16,
    key: String,
}

// The job handle is only closed, never shared.
unsafe impl Send for Server {}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = unsafe { CloseHandle(self.job) };
    }
}

#[derive(Default)]
struct Chat {
    passages: Vec<Passage>,
    /// (role, content) after the system prompt.
    messages: Vec<(String, String)>,
}

pub struct Ai {
    /// The local model's file name, for questions about files.
    model: String,
    /// The cloud API for general questions, by name ("" for whichever has a key), and its model.
    api: String,
    api_model: String,
    server: Mutex<Option<Server>>,
    used: Mutex<Instant>,
    chat: Mutex<Chat>,
    /// The general conversation: (role, content) after its system prompt.
    general: Mutex<Vec<(String, String)>>,
    /// The question being answered; a newer number (another question, or Stop) ends it.
    asking: AtomicU64,
}

impl Ai {
    pub fn new(config: &common::Config) -> Self {
        if !config.api.is_empty() && !APIS.iter().any(|a| a.name == config.api) {
            tracing::warn!(
                "api = {:?} isn't known; using whichever key is set",
                config.api
            );
        }
        Self {
            model: if config.ai_model.is_empty() {
                DEFAULT_MODEL.to_string()
            } else {
                config.ai_model.clone()
            },
            api: config.api.clone(),
            api_model: config.api_model.clone(),
            server: Mutex::new(None),
            used: Mutex::new(Instant::now()),
            chat: Mutex::default(),
            general: Mutex::default(),
            asking: AtomicU64::new(0),
        }
    }

    /// The cloud API for general questions, its model and key: the one config.toml names, or
    /// else the first whose key is set. Keys are read at each question, so one set after Seekr
    /// started works without a restart.
    fn cloud(&self) -> Result<(&'static Api, String, String), String> {
        let api = APIS
            .iter()
            .find(|a| a.name == self.api)
            .or_else(|| APIS.iter().find(|a| user_env(a.key).is_some()))
            .ok_or("set GEMINI_API_KEY or XAI_API_KEY in your environment for general questions")?;
        let key = user_env(api.key)
            .ok_or_else(|| format!("{} isn't set in your environment", api.key))?;
        let model = if self.api_model.is_empty() {
            api.model.to_string()
        } else {
            self.api_model.clone()
        };
        Ok((api, model, key))
    }
}

/// `?question` (about the user's files) or `??question` (anything): whether it is general, and
/// the question.
pub fn mode(text: &str) -> Option<(bool, &str)> {
    let text = text.trim_start();
    if let Some(rest) = text.strip_prefix("??") {
        return Some((true, rest.trim()));
    }
    text.strip_prefix('?').map(|rest| (false, rest.trim()))
}

pub fn items(app: &AppHandle, general: bool, question: &str) -> Vec<Item> {
    let Some(ai) = app.try_state::<Ai>() else {
        return Vec::new();
    };
    let (title, subtitle, preview) = if general {
        let subtitle = match ai.cloud() {
            Ok((api, model, _)) => format!("{} · {model} · only your question is sent", api.label),
            Err(e) => format!("To ask, {e}"),
        };
        let title = if question.is_empty() {
            "Ask AI anything".to_string()
        } else {
            format!("Ask AI: {question}")
        };
        let preview = "General questions go to a cloud model: only what you type after ?? is \
            sent, never your files. Ask a follow-up with ?? again; Esc stops an answer.";
        (title, subtitle, preview)
    } else {
        let subtitle = if model_path(&ai.model).exists() && runtime().exists() {
            format!("Your files · on this PC · {}", ai.model.replace('-', " "))
        } else {
            "The AI model isn't installed yet".to_string()
        };
        let title = if question.is_empty() {
            "Ask AI about your files".to_string()
        } else {
            format!("Ask your files: {question}")
        };
        let preview = "Answers come from the files content search reads, with the passages \
            cited, from a model on this PC; nothing leaves it. Ask a follow-up the same way; Esc \
            stops an answer. For anything else, start with ??.";
        (title, subtitle, preview)
    };
    vec![Item {
        id: "ai:ask".into(),
        title,
        subtitle,
        preview: preview.into(),
        icon: None,
        line: None,
        actions: vec![ASK],
    }]
}

fn runtime() -> PathBuf {
    common::data_dir().join(RUNTIME)
}

fn model_path(model: &str) -> PathBuf {
    common::data_dir()
        .join(MODELS)
        .join(format!("{model}.gguf"))
}

#[derive(Clone, Serialize)]
pub struct Source {
    n: usize,
    path: String,
    line: u32,
    name: String,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AiEvent {
    Sources { sources: Vec<Source> },
    Status { text: String },
    Token { text: String },
    Done { seconds: f64, first: f64 },
    Error { text: String },
}

#[tauri::command]
pub async fn ask(
    app: AppHandle,
    question: String,
    general: bool,
    follow_up: bool,
    on_event: Channel<AiEvent>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let ai = app.state::<Ai>();
        let id = ai.asking.fetch_add(1, Ordering::SeqCst) + 1;
        let answered = if general {
            answer_general(&ai, id, &question, follow_up, &on_event)
        } else {
            answer(&app, &ai, id, &question, follow_up, &on_event)
        };
        if let Err(e) = answered {
            tracing::warn!("AI answer failed: {e}");
            let _ = on_event.send(AiEvent::Error { text: e });
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Stops the answer being written.
#[tauri::command]
pub fn stop_answer(app: AppHandle) {
    if let Some(ai) = app.try_state::<Ai>() {
        ai.asking.fetch_add(1, Ordering::SeqCst);
    }
}

fn answer(
    app: &AppHandle,
    ai: &Ai,
    id: u64,
    question: &str,
    follow_up: bool,
    events: &Channel<AiEvent>,
) -> Result<(), String> {
    let send = |e: AiEvent| {
        let _ = events.send(e);
    };
    *ai.used.lock().unwrap() = Instant::now();
    {
        let mut chat = ai.chat.lock().unwrap();
        if !follow_up || chat.messages.is_empty() {
            *chat = Chat::default();
        }
        // A follow-up is read with the question before it, which it often leans on.
        let previous = chat
            .messages
            .iter()
            .rev()
            .find(|m| m.0 == "user")
            .map(|m| m.1.rsplit("Question: ").next().unwrap_or("").to_string())
            .unwrap_or_default();
        let budget = if chat.messages.is_empty() {
            FIRST_BUDGET
        } else {
            FOLLOW_UP_BUDGET
        };
        let found = content::retrieve(
            app,
            &format!("{previous} {question}"),
            budget,
            &chat.passages,
        );
        let first = chat.passages.len();
        chat.passages.extend(found);
        let fresh = &chat.passages[first..];
        let numbered: Vec<String> = fresh
            .iter()
            .enumerate()
            .map(|(i, p)| {
                format!(
                    "[{}] (from {}, line {})\n{}",
                    first + i + 1,
                    name(&p.path),
                    p.line,
                    p.text
                )
            })
            .collect();
        let user = match (numbered.is_empty(), first) {
            (true, 0) => {
                format!("There are no passages from the user's files.\n\nQuestion: {question}")
            }
            (true, _) => format!("Question: {question}"),
            (false, 0) => format!(
                "Passages:\n\n{}\n\nQuestion: {question}",
                numbered.join("\n\n")
            ),
            (false, _) => format!(
                "More passages:\n\n{}\n\nQuestion: {question}",
                numbered.join("\n\n")
            ),
        };
        chat.messages.push(("user".into(), user));
        send(AiEvent::Sources {
            sources: chat
                .passages
                .iter()
                .enumerate()
                .map(|(i, p)| Source {
                    n: i + 1,
                    path: p.path.clone(),
                    line: p.line,
                    name: name(&p.path),
                })
                .collect(),
        });
    }
    let messages: Vec<Value> = std::iter::once(json!({"role": "system", "content": SYSTEM}))
        .chain(
            ai.chat
                .lock()
                .unwrap()
                .messages
                .iter()
                .map(|(role, content)| json!({"role": role, "content": content})),
        )
        .collect();
    // Questions about files only ever go to the model on this PC.
    let (port, key) = local_server(ai, &ai.model, &send)?;
    let mut body = json!({
        "chat_template_kwargs": {"enable_thinking": false},
        "cache_prompt": true,
    });
    body["messages"] = Value::Array(messages);
    body["stream"] = json!(true);
    body["temperature"] = json!(0.2);
    body["max_tokens"] = json!(MAX_ANSWER_TOKENS);
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let (text, first, total) = stream_reply(ai, id, &url, &key, body, None, &send)?;
    *ai.used.lock().unwrap() = Instant::now();
    ai.chat
        .lock()
        .unwrap()
        .messages
        .push(("assistant".into(), text));
    send(AiEvent::Done {
        seconds: total.as_secs_f64(),
        first: first.as_secs_f64(),
    });
    Ok(())
}

/// A general question for the cloud model: just the question and the conversation so far.
fn answer_general(
    ai: &Ai,
    id: u64,
    question: &str,
    follow_up: bool,
    events: &Channel<AiEvent>,
) -> Result<(), String> {
    let send = |e: AiEvent| {
        let _ = events.send(e);
    };
    let (api, model, key) = ai.cloud()?;
    let mut history = ai.general.lock().unwrap().clone();
    if !follow_up {
        history.clear();
    }
    history.push(("user".into(), question.to_string()));
    let messages: Vec<Value> = std::iter::once(json!({"role": "system", "content": GENERAL}))
        .chain(
            history
                .iter()
                .map(|(r, c)| json!({"role": r, "content": c})),
        )
        .collect();
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "temperature": 0.3,
        "max_tokens": MAX_ANSWER_TOKENS * 2,
    });
    if let Some((field, value)) = api.no_thinking {
        body[field] = json!(value);
    }
    let (text, first, total) = stream_reply(ai, id, api.url, &key, body, api.no_thinking, &send)?;
    history.push(("assistant".into(), text));
    *ai.general.lock().unwrap() = history;
    send(AiEvent::Done {
        seconds: total.as_secs_f64(),
        first: first.as_secs_f64(),
    });
    Ok(())
}

/// Posts a chat request and passes on the answer as it streams: (answer, time to its first
/// word, time in all). Stops early once question `id` is no longer the one being answered.
/// `optional` names a field the API may refuse; the request is sent again without it.
fn stream_reply(
    ai: &Ai,
    id: u64,
    url: &str,
    key: &str,
    mut body: Value,
    optional: Option<(&str, &str)>,
    send: &impl Fn(AiEvent),
) -> Result<(String, Duration, Duration), String> {
    let start = Instant::now();
    let mut first = None;
    let mut text = String::new();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into();
    let post = |body: &Value| {
        agent
            .post(url)
            .header("Authorization", &format!("Bearer {key}"))
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .map_err(|e| format!("the model didn't answer: {e}"))
    };
    let mut response = post(&body)?;
    // A model that can't turn thinking off refuses the field; it can still answer without it.
    if response.status() == 400
        && let Some((field, _)) = optional
        && let Some(fields) = body.as_object_mut()
    {
        fields.remove(field);
        response = post(&body)?;
    }
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.body_mut().read_to_string().unwrap_or_default();
        tracing::warn!(
            "AI request failed with {status}: {}",
            detail.chars().take(300).collect::<String>()
        );
        return Err(format!("the model refused the request ({status})"));
    }
    let reader = BufReader::new(response.body_mut().as_reader());
    for line in reader.lines() {
        if ai.asking.load(Ordering::SeqCst) != id {
            // Dropping the connection stops generation on the server.
            break;
        }
        let line = line.map_err(|e| e.to_string())?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let delta = event["choices"][0]["delta"]["content"]
            .as_str()
            .unwrap_or("");
        if !delta.is_empty() {
            first.get_or_insert_with(|| start.elapsed());
            text.push_str(delta);
            send(AiEvent::Token {
                text: delta.to_string(),
            });
        }
    }
    Ok((text, first.unwrap_or_default(), start.elapsed()))
}

fn name(path: &str) -> String {
    path.rsplit('\\').next().unwrap_or(path).to_string()
}

/// The running local server's port and key, starting it if needed.
fn local_server(ai: &Ai, model: &str, send: &impl Fn(AiEvent)) -> Result<(u16, String), String> {
    let mut server = ai.server.lock().unwrap();
    if let Some(s) = server.as_mut() {
        if s.child.try_wait().map_err(|e| e.to_string())?.is_none() {
            return Ok((s.port, s.key.clone()));
        }
        tracing::warn!("the AI server had stopped; starting it again");
        *server = None;
    }
    let (runtime, model_file) = (runtime(), model_path(model));
    if !runtime.exists() || !model_file.exists() {
        return Err("the AI model isn't installed yet".into());
    }
    send(AiEvent::Status {
        text: "Starting the AI model…".into(),
    });
    let start = Instant::now();
    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_err(|e| e.to_string())?
        .port();
    // Other programs on this PC can reach a local port, so requests must carry this key.
    let key = format!(
        "{:016x}{:016x}",
        std::hash::BuildHasher::hash_one(&std::collections::hash_map::RandomState::new(), start),
        std::hash::BuildHasher::hash_one(&std::collections::hash_map::RandomState::new(), port)
    );
    let log = std::fs::File::create(common::data_dir().join("logs").join("llama-server.log"))
        .map_err(|e| e.to_string())?;
    let child = Command::new(&runtime)
        .args(["-m"])
        .arg(&model_file)
        .args([
            "-ngl",
            "99",
            "-fa",
            "on",
            "-ctk",
            "q8_0",
            "-ctv",
            "q8_0",
            "-c",
            CONTEXT_TOKENS,
        ])
        .args([
            "--jinja",
            "--no-webui",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
        .args(["--api-key", &key])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("couldn't start the AI model: {e}"))?;
    let job = kill_with_seekr(child.id()).map_err(|e| e.to_string())?;
    let started = Server {
        child,
        job,
        port,
        key: key.clone(),
    };
    // Loading takes a few seconds; health answers 200 once the model is ready.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(2)))
        .http_status_as_error(false)
        .build()
        .into();
    let health = format!("http://127.0.0.1:{port}/health");
    loop {
        if start.elapsed() > Duration::from_secs(90) {
            return Err("the AI model took too long to start".into());
        }
        if let Ok(r) = agent.get(&health).call()
            && r.status().is_success()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    tracing::info!("AI model {model} ready in {:?}", start.elapsed());
    *server = Some(started);
    Ok((port, key))
}

/// A job object that ends the server when Seekr's handle to it closes, even if Seekr crashes.
fn kill_with_seekr(pid: u32) -> windows::core::Result<HANDLE> {
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null())?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )?;
        let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid)?;
        let assigned = AssignProcessToJobObject(job, process);
        let _ = CloseHandle(process);
        assigned?;
        Ok(job)
    }
}

/// Stops the local model once it has been idle for ten minutes, handing the GPU back.
pub fn start_reaper(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(30));
            let Some(ai) = app.try_state::<Ai>() else {
                return;
            };
            let idle = ai.used.lock().unwrap().elapsed() > IDLE;
            let mut server = ai.server.lock().unwrap();
            if idle && server.is_some() {
                *server = None;
                tracing::info!(
                    "AI model stopped after {} idle minutes",
                    IDLE.as_secs() / 60
                );
            }
        }
    });
}

/// A variable from the process, or else from the user's saved environment (set after Seekr
/// started, so not inherited). Read, never logged.
fn user_env(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name)
        && !value.is_empty()
    {
        return Some(value);
    }
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Environment"),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if read.is_err() {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..len])).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu_memory_mb() -> u32 {
        let out = Command::new("nvidia-smi")
            .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0)
    }

    /// Runs the local model the way the app does, without the panel:
    /// `cargo test -p seekr live_local_answer -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_local_answer() {
        let ai = Ai::new(&common::Config::default());
        let idle_gpu = gpu_memory_mb();
        let started = Instant::now();
        let (port, key) = local_server(&ai, DEFAULT_MODEL, &|_| {}).unwrap();
        println!(
            "server ready in {:?}, GPU memory {} MB",
            started.elapsed(),
            gpu_memory_mb()
        );
        let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
        let passages = "Passages:\n\n[1] (from README.md, line 17)\nThen press Alt+Space.\n\n\
            [2] (from CHANGELOG.md, line 40)\nClipboard history keeps what you copy for 30 days \
            or 1,000 entries.";
        let body = |question: &str, max: u32| {
            json!({
                "messages": [
                    {"role": "system", "content": SYSTEM},
                    {"role": "user", "content": format!("{passages}\n\nQuestion: {question}")},
                ],
                "chat_template_kwargs": {"enable_thinking": false},
                "cache_prompt": true, "stream": true, "temperature": 0.2, "max_tokens": max,
            })
        };
        let tokens = std::sync::atomic::AtomicUsize::new(0);
        let count = |e: AiEvent| {
            if matches!(e, AiEvent::Token { .. }) {
                tokens.fetch_add(1, Ordering::SeqCst);
            }
        };

        let id = ai.asking.fetch_add(1, Ordering::SeqCst) + 1;
        let (text, first, total) = stream_reply(
            &ai,
            id,
            &url,
            &key,
            body("Which keys open the Seekr panel?", 200),
            None,
            &count,
        )
        .unwrap();
        println!("answer in {first:?} to the first word, {total:?} in all: {text}");
        assert!(text.contains("Alt+Space"), "{text}");
        assert!(text.contains("[1]"), "cites the passage: {text}");
        let id = ai.asking.fetch_add(1, Ordering::SeqCst) + 1;
        let (text, first, _) = stream_reply(
            &ai,
            id,
            &url,
            &key,
            body("How long is clipboard history kept?", 200),
            None,
            &count,
        )
        .unwrap();
        println!("second answer in {first:?} to the first word: {text}");
        assert!(text.contains("30 days"), "{text}");

        // Stop: a newer question number ends the stream within a token or two.
        tokens.store(0, Ordering::SeqCst);
        let id = ai.asking.fetch_add(1, Ordering::SeqCst) + 1;
        let stopper = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(700));
                ai.asking.fetch_add(1, Ordering::SeqCst);
            });
            stream_reply(
                &ai,
                id,
                &url,
                &key,
                body("Write a 500-word essay about launchers.", 600),
                None,
                &count,
            )
        });
        let (essay, _, total) = stopper.unwrap();
        println!(
            "stopped after {total:?} and {} tokens",
            tokens.load(Ordering::SeqCst)
        );
        assert!(total < Duration::from_secs(3) && essay.len() < 1500);

        // Closing Seekr's job handle ends the server, as when Seekr exits or crashes.
        let mut server = ai.server.lock().unwrap().take().unwrap();
        let _ = unsafe { CloseHandle(server.job) };
        server.job = HANDLE::default();
        std::thread::sleep(Duration::from_secs(2));
        assert!(
            server.child.try_wait().unwrap().is_some(),
            "server ended with its job"
        );
        drop(server);
        std::thread::sleep(Duration::from_secs(2));
        let after = gpu_memory_mb();
        println!("GPU memory {idle_gpu} MB before, {after} MB after");
        assert!(after < idle_gpu + 300, "the GPU memory came back");
    }

    #[test]
    fn question_mode() {
        assert_eq!(
            mode("?what is due friday"),
            Some((false, "what is due friday"))
        );
        assert_eq!(mode("  ? hi "), Some((false, "hi")));
        assert_eq!(mode("??capital of peru"), Some((true, "capital of peru")));
        assert_eq!(mode("??"), Some((true, "")));
        assert_eq!(mode("what?"), None);
    }

    #[test]
    fn general_questions_need_a_key_and_name_it() {
        let ai = Ai::new(&common::Config {
            api: "grok".into(),
            ..Default::default()
        });
        if user_env("XAI_API_KEY").is_none() {
            assert_eq!(
                ai.cloud().unwrap_err(),
                "XAI_API_KEY isn't set in your environment"
            );
        }
    }
}
