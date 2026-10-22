//! Passage and query embeddings from gte-modernbert-base (768 dimensions, CLS pooling, unit
//! length), chosen by `docs/benchmarks/embeddings.md`: the fp32 weights for documents, in bulk on
//! a GPU through DirectML, and the int8 weights for queries on the CPU.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use ort::{
    environment::Environment,
    ep,
    session::{Session, builder::GraphOptimizationLevel},
    value::Tensor,
};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

pub const DIMS: usize = 768;
pub const MAX_TOKENS: usize = 512;
/// DirectML compiles a GPU graph for every new input shape, which costs more than running it;
/// padding sequences to a multiple of this and every batch to `GPU_BATCH` keeps it to four.
const PAD_TO: usize = 128;
pub const GPU_BATCH: usize = 16;
/// Hugging Face revision every model file is pinned to.
pub const REVISION: &str = "e7f32e3c00f91d699e8c43b53106206bcc72bb22";

pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weights {
    /// For documents: the best quality.
    Full,
    /// For queries: a quarter of the size and half the time, 2 points of nDCG lower.
    Int8,
}

impl Weights {
    fn file(self) -> &'static str {
        match self {
            Weights::Full => r"onnx\model.onnx",
            Weights::Int8 => r"onnx\model_int8.onnx",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Cpu,
    /// A DirectML adapter, by its DXGI enumeration index.
    DirectMl(i32),
}

/// `%LOCALAPPDATA%\Seekr\models\gte-modernbert-base`, laid out as on Hugging Face.
pub fn model_dir() -> PathBuf {
    common::data_dir()
        .join("models")
        .join("gte-modernbert-base")
}

/// Whether the model files for `weights` are installed.
pub fn available(weights: Weights) -> bool {
    let dir = model_dir();
    dir.join(weights.file()).is_file() && dir.join("tokenizer.json").is_file()
}

/// The ONNX Runtime environment, one per process.
fn environment() -> Result<&'static Environment, Error> {
    static ENV: OnceLock<Environment> = OnceLock::new();
    if let Some(env) = ENV.get() {
        return Ok(env);
    }
    let env = ort::init().with_name("seekr").build()?;
    // At info it logs every allocation.
    env.set_log_level(ort::logging::LogLevel::Warning);
    Ok(ENV.get_or_init(|| env))
}

pub struct Embedder {
    session: Session,
    tokenizer: Tokenizer,
    token_types: bool,
    /// Pad every batch to this many texts (on a GPU).
    fixed_batch: Option<usize>,
}

impl Embedder {
    pub fn load(weights: Weights, device: Device) -> Result<Self, Error> {
        Self::load_from(&model_dir(), weights, device)
    }

    pub fn load_from(dir: &Path, weights: Weights, device: Device) -> Result<Self, Error> {
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))?;
        tokenizer.with_truncation(Some(TruncationParams {
            max_length: MAX_TOKENS,
            ..Default::default()
        }))?;
        tokenizer.with_padding(Some(PaddingParams {
            pad_to_multiple_of: Some(PAD_TO),
            ..Default::default()
        }));
        // The builder's errors carry the builder, which isn't Send; keep just the message.
        let msg = |e: ort::Error<ort::session::builder::SessionBuilder>| e.to_string();
        let mut builder = Session::builder(environment()?)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(msg)?
            // Background work: leave most of the CPU to everything else.
            .with_intra_threads(4)
            .map_err(msg)?;
        if let Device::DirectMl(adapter) = device {
            // DirectML needs fixed memory patterns off.
            builder = builder
                .with_memory_pattern(false)
                .map_err(msg)?
                .with_execution_providers([ep::DirectML::default()
                    .with_device_id(adapter)
                    .build()
                    .error_on_failure()])
                .map_err(msg)?;
        }
        let session = builder.commit_from_file(dir.join(weights.file()))?;
        let token_types = session
            .inputs()
            .iter()
            .any(|i| i.name() == "token_type_ids");
        Ok(Self {
            session,
            tokenizer,
            token_types,
            fixed_batch: matches!(device, Device::DirectMl(_)).then_some(GPU_BATCH),
        })
    }

    /// One unit-length vector per text, in order.
    pub fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let wanted = texts.len();
        let mut texts = texts.to_vec();
        if let Some(n) = self.fixed_batch {
            texts.resize(texts.len().max(n), "");
        }
        let encodings = self.tokenizer.encode_batch(texts, true)?;
        let (rows, cols) = (encodings.len(), encodings[0].len());
        let mut ids = Vec::with_capacity(rows * cols);
        let mut mask = Vec::with_capacity(rows * cols);
        for e in &encodings {
            ids.extend(e.get_ids().iter().map(|&t| i64::from(t)));
            mask.extend(e.get_attention_mask().iter().map(|&m| i64::from(m)));
        }
        let shape = [rows as i64, cols as i64];
        let mut inputs = ort::inputs![
            "input_ids" => Tensor::from_array((shape, ids))?,
            "attention_mask" => Tensor::from_array((shape, mask))?,
        ];
        if self.token_types {
            inputs.push((
                "token_type_ids".into(),
                Tensor::from_array((shape, vec![0i64; rows * cols]))?.into(),
            ));
        }
        let outputs = self.session.run(inputs)?;
        let (shape, hidden) = outputs[0].try_extract_tensor::<f32>()?;
        let (seq, dims) = (shape[1] as usize, shape[2] as usize);
        Ok((0..wanted)
            .map(|r| {
                // The CLS token's state is the embedding.
                let cls = &hidden[r * seq * dims..r * seq * dims + dims];
                let norm = cls.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
                cls.iter().map(|x| x / norm).collect()
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test -p content --release real_model -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_model_ranks_meaning_on_every_device() {
        let texts = [
            "notes from our trip to the lighthouse on the coast",
            "travel journal: visiting the seaside beacon",
            "quarterly tax return for the small business",
        ];
        for (weights, device) in [
            (Weights::Int8, Device::Cpu),
            (Weights::Full, Device::Cpu),
            (Weights::Full, Device::DirectMl(0)),
        ] {
            let mut model = Embedder::load(weights, device).unwrap();
            let start = std::time::Instant::now();
            let v = model.embed(&texts).unwrap();
            let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
            let (near, far) = (dot(&v[0], &v[1]), dot(&v[0], &v[2]));
            println!(
                "{weights:?} {device:?}: near {near:.3} far {far:.3} in {:?}",
                start.elapsed()
            );
            assert_eq!(v[0].len(), DIMS);
            assert!(near > far + 0.1);
        }
    }
}
