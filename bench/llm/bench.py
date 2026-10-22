"""Picks the model and runtime for Seekr's AI answers.

Each candidate answers the same 22 questions about Seekr from its own README and CHANGELOG at a
fixed commit, given about 3,000 tokens of numbered passages per question (the passages sharing
the most words with it), and asked to cite them. No model can know these answers beforehand, and
two have no answer in the passages, which a grounded model should say.

Measured: time to first token cold (runtime started, model not loaded) and warm, generation
speed, total time, answers containing the expected facts, citations naming a passage that holds
them, and the RTX 5050's peak memory and mean power while answering.

    python bench/llm/bench.py [candidate ...]

Candidates: ollama (qwen3.5:9b as installed), llama-9b-q4km (the same GGUF in llama-server),
llama-9b-iq4xs, llama-4b-q4km, gemini and groq (need GEMINI_API_KEY or GROQ_API_KEY in the
user's environment; SEEKR_API_MODEL overrides the model).
Results go to bench/llm/results.json.
"""

import json
import os
import re
import statistics
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LLM = Path(os.environ["LOCALAPPDATA"]) / "Seekr" / "llm"
LLAMA = LLM / "llama.cpp-b11819" / "llama-server.exe"
OLLAMA_BLOB = Path.home() / ".ollama/models/blobs/sha256-02d45dc1cf451ba2475ac33b301c2dd8f985abe4c182ce04a1f2f5bf0260278d"
COMMIT = "eaa6dc8"
CONTEXT_CHARS = 12_000  # about 3,000 tokens
MAX_TOKENS = 400
PORT = 8090
# Extra llama-server flags to try, such as "-ub 2048"; results are stored under the name plus them.
EXTRA_ARGS = os.environ.get("SEEKR_LLAMA_ARGS", "").split()

QUESTIONS = [
    ("Which keys open the Seekr panel?", ["Alt+Space"]),
    ("How long does clipboard history keep entries, and at most how many?", ["30 days", "1,000"]),
    ("In which folder does Seekr save notes?", ["Seekr Notes"]),
    ("What does typing `yt lofi` do?", ["YouTube"]),
    ("How big is the meaning-search model download?", ["750 MB"]),
    ("Which embedding model does meaning search use?", ["gte-modernbert-base"]),
    ("What happens when Seekr would paste into an admin window?", ["Ctrl+V"]),
    ("At most how often are browser bookmarks and history read again?", ["five minutes"]),
    ("Which file holds the to-do list?", ["To-dos.md"]),
    ("Which install.ps1 switch installs the app without the service?", ["-SkipService"]),
    ("Which browsers' bookmarks and history does Seekr read?", ["Brave", "Chrome", "Edge"]),
    ("How much does a Seekr licence cost?", None),  # not in the passages
    # Harder: an exact number among many, or several facts at once.
    ("What p95 keystroke latency was measured while typing calculations?", ["15.0 ms"]),
    ("How many bookmarks and history entries were read, and how long did it take?",
     ["61", "5,003", "157 ms"]),
    ("How does Seekr set brightness on the built-in panel and on external monitors?",
     ["WMI", "DDC/CI"]),
    ("How long after the last search is the query model dropped, and how long does loading it take?",
     ["30 s", "0.5 s"]),
    ("Which key saves a note being edited in the preview pane?", ["Ctrl+S"]),
    ("What happens to a file that crashed or hung the indexer?", ["quarantin"]),
    ("How many Windows Settings pages can be opened from Seekr?", ["106"]),
    ("Which Unicode CLDR version do emoji names and keywords come from?", ["48"]),
    ("How long did the first embedding of about 9,000 passages take on the RTX 5050?", ["5.6 min"]),
    ("Who designed Seekr's logo?", None),  # not in the passages
]

SYSTEM = (
    "Answer the question using only the numbered passages. Be brief: one or two sentences. "
    "Cite the passages you used like [2]. If the passages don't contain the answer, say you "
    "don't know."
)


def passages() -> list[str]:
    """README and CHANGELOG at COMMIT, cut into paragraphs (long bullets on their own)."""
    out = []
    for name in ("README.md", "CHANGELOG.md"):
        text = subprocess.run(["git", "show", f"{COMMIT}:{name}"], cwd=ROOT, capture_output=True,
                              text=True, encoding="utf-8", check=True).stdout
        for block in re.split(r"\n\s*\n|\n(?=- )", text):
            block = block.strip()
            if len(block) > 40:
                out.append(block)
    return out


def words(text: str) -> set[str]:
    return {w for w in re.findall(r"[a-z0-9+.-]{3,}", text.lower())}


def context_for(question: str, all_passages: list[str]) -> list[str]:
    """The passages sharing the most words with the question, up to CONTEXT_CHARS."""
    q = words(question)
    ranked = sorted(all_passages, key=lambda p: -len(q & words(p)))
    chosen, size = [], 0
    for p in ranked:
        if size + len(p) > CONTEXT_CHARS:
            continue
        chosen.append(p)
        size += len(p)
    return chosen


def prompt(question: str, chosen: list[str]) -> str:
    numbered = "\n\n".join(f"[{i + 1}] {p}" for i, p in enumerate(chosen))
    return f"Passages:\n\n{numbered}\n\nQuestion: {question}"


class Gpu(threading.Thread):
    """Samples the NVIDIA GPU's memory and power every 200 ms."""

    def __init__(self):
        super().__init__(daemon=True)
        self.samples: list[tuple[float, float]] = []
        self.proc = subprocess.Popen(
            ["nvidia-smi", "--query-gpu=memory.used,power.draw", "--format=csv,noheader,nounits",
             "-lms", "200"], stdout=subprocess.PIPE, text=True)

    def run(self):
        for line in self.proc.stdout:
            try:
                mem, power = (float(x) for x in line.split(","))
                self.samples.append((mem, power))
            except ValueError:
                pass

    def stop(self) -> tuple[float, float]:
        self.proc.terminate()
        if not self.samples:
            return 0.0, 0.0
        return max(m for m, _ in self.samples), statistics.mean(p for _, p in self.samples)


def stream(url: str, body: dict, headers: dict, sse: bool) -> tuple[str, float, float, int]:
    """Posts a streaming chat request: (answer, seconds to first token, seconds in total, tokens)."""
    request = urllib.request.Request(url, data=json.dumps(body).encode(), method="POST",
                                     headers={"Content-Type": "application/json", **headers})
    start = time.perf_counter()
    first, answer, tokens = None, [], 0
    with urllib.request.urlopen(request, timeout=600) as response:
        for raw in response:
            line = raw.decode("utf-8").strip()
            if sse:
                if not line.startswith("data:") or line == "data: [DONE]":
                    continue
                event = json.loads(line[5:])
                delta = "".join((c.get("delta") or {}).get("content") or "" for c in event.get("choices", []))
                usage = event.get("usage") or {}
                tokens = usage.get("completion_tokens") or tokens
            else:
                if not line:
                    continue
                event = json.loads(line)
                delta = (event.get("message") or {}).get("content") or ""
                tokens = event.get("eval_count") or tokens
            if delta:
                first = first or time.perf_counter()
                answer.append(delta)
    end = time.perf_counter()
    text = "".join(answer)
    return text, (first or end) - start, end - start, tokens or max(1, len(text) // 4)


def wait_ready(url: str, seconds: int):
    deadline = time.time() + seconds
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return
        except Exception:
            time.sleep(0.3)
    raise TimeoutError(url)


def user_key(name: str) -> str:
    key = os.environ.get(name)
    if not key:
        # Set with [Environment]::SetEnvironmentVariable(..., 'User'); read without printing it.
        key = subprocess.run(["powershell", "-NoProfile", "-Command",
                              f"[Environment]::GetEnvironmentVariable('{name}','User')"],
                             capture_output=True, text=True).stdout.strip()
    if not key:
        raise SystemExit(f"{name} isn't set in your user environment")
    return key


def run(name: str, ask, items) -> dict:
    """Asks every question; the first answer is the cold one."""
    gpu = Gpu()
    gpu.start()
    rows = []
    for i, (question, expected, chosen) in enumerate(items):
        text, ttft, total, tokens = ask(prompt(question, chosen))
        gen = total - ttft
        cited = {int(n) for n in re.findall(r"\[(\d+)\]", text)}
        holders = {i + 1 for i, p in enumerate(chosen) if expected and all(e in p for e in expected)}
        if expected is None:
            correct = bool(re.search(r"don.t know|not (mention|contain|say|state|provide|given|specif)|no (information|mention)", text, re.I))
            cites_right = None
        else:
            correct = all(e.lower() in text.lower() for e in expected)
            cites_right = bool(cited & holders) if holders else None
        rows.append({"question": question, "answer": text, "ttft": ttft, "total": total,
                     "tokens": tokens, "tok_s": tokens / gen if gen > 0 else 0,
                     "correct": correct, "cites_right": cites_right, "cold": i == 0})
        print(f"  {name} q{i + 1}: {ttft:5.2f}s first, {total:5.2f}s total, "
              f"{rows[-1]['tok_s']:5.1f} tok/s, correct={correct} cites={cites_right}", flush=True)
    vram, power = gpu.stop()
    warm = [r for r in rows if not r["cold"]]
    cites = [r["cites_right"] for r in rows if r["cites_right"] is not None]
    return {
        "cold_ttft": rows[0]["ttft"],
        "warm_ttft_p50": statistics.median(r["ttft"] for r in warm),
        "warm_ttft_max": max(r["ttft"] for r in warm),
        "tok_s": statistics.median(r["tok_s"] for r in warm),
        "total_p50": statistics.median(r["total"] for r in warm),
        "correct": sum(r["correct"] for r in rows),
        "questions": len(rows),
        "citations_right": f"{sum(cites)}/{len(cites)}",
        "vram_peak_mb": vram,
        "power_mean_w": power,
        "rows": rows,
    }


def llama(model: Path):
    def start():
        proc = subprocess.Popen([str(LLAMA), "-m", str(model), "-ngl", "99", "-fa", "on",
                                 "-ctk", "q8_0", "-ctv", "q8_0", "-c", "8192", "--jinja",
                                 "--port", str(PORT), "--no-webui", *EXTRA_ARGS],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return proc

    def ask(text):
        body = {"messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": text}],
                "stream": True, "max_tokens": MAX_TOKENS, "temperature": 0.2,
                "stream_options": {"include_usage": True},
                "chat_template_kwargs": {"enable_thinking": False}}
        return stream(f"http://127.0.0.1:{PORT}/v1/chat/completions", body, {}, sse=True)
    return start, ask


def ollama():
    def start():
        # Unloads the model, so the first question pays for loading it.
        subprocess.run(["ollama", "stop", "qwen3.5:9b"], capture_output=True)
        time.sleep(2)
        return None

    def ask(text):
        body = {"model": "qwen3.5:9b", "stream": True, "think": False,
                "options": {"num_ctx": 8192, "temperature": 0.2, "num_predict": MAX_TOKENS},
                "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": text}]}
        return stream("http://127.0.0.1:11434/api/chat", body, {}, sse=False)
    return start, ask


def cloud(url: str, key_name: str, model: str, extra: dict):
    key = user_key(key_name)

    def start():
        return None

    def ask(text):
        body = {"model": model, "stream": True, "max_tokens": MAX_TOKENS, "temperature": 0.2,
                "stream_options": {"include_usage": True}, **extra,
                "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": text}]}
        return stream(url, body, {"Authorization": f"Bearer {key}"}, sse=True)
    return start, ask


CANDIDATES = {
    "ollama": lambda: ollama(),
    "llama-9b-q4km": lambda: llama(OLLAMA_BLOB),
    "llama-9b-iq4xs": lambda: llama(LLM / "models" / "Qwen3.5-9B-IQ4_XS.gguf"),
    "llama-4b-q4km": lambda: llama(LLM / "models" / "Qwen3.5-4B-Q4_K_M.gguf"),
    "gemini": lambda: cloud("https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
                            "GEMINI_API_KEY", os.environ.get("SEEKR_API_MODEL", "gemini-2.5-flash"),
                            {"reasoning_effort": "none"}),
    "groq": lambda: cloud("https://api.groq.com/openai/v1/chat/completions", "GROQ_API_KEY",
                          os.environ.get("SEEKR_API_MODEL", "openai/gpt-oss-120b"),
                          {"reasoning_effort": "low"}),
}


def main():
    names = sys.argv[1:] or list(CANDIDATES)
    all_passages = passages()
    items = [(q, e, context_for(q, all_passages)) for q, e in QUESTIONS]
    for q, e, chosen in items:
        if e and not any(all(x in p for x in e) for p in chosen):
            print(f"warning: no passage given for '{q}' holds {e}")
    results_path = Path(__file__).with_name("results.json")
    results = json.loads(results_path.read_text()) if results_path.exists() else {}
    for name in names:
        start, ask = CANDIDATES[name]()
        launched = time.perf_counter()
        proc = start()
        try:
            if proc is not None:
                wait_ready(f"http://127.0.0.1:{PORT}/health", 300)
                # Cold here is runtime start to first token, model load included.
                load = time.perf_counter() - launched
            else:
                load = 0.0
            result = run(name, ask, items)
            result["cold_ttft"] += load
            result["load_s"] = load
            results[" ".join([name, *EXTRA_ARGS]) if name.startswith("llama") else name] = result
            print(f"{name}: cold {result['cold_ttft']:.2f}s, warm p50 {result['warm_ttft_p50']:.2f}s, "
                  f"{result['tok_s']:.1f} tok/s, {result['correct']}/{result['questions']} right, "
                  f"citations {result['citations_right']}, VRAM {result['vram_peak_mb']:.0f} MB, "
                  f"{result['power_mean_w']:.0f} W", flush=True)
        finally:
            if proc is not None:
                proc.terminate()
                proc.wait(30)
        results_path.write_text(json.dumps(results, indent=1, ensure_ascii=False))
        time.sleep(5)


if __name__ == "__main__":
    main()
