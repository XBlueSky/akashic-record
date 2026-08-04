use anyhow::{Context, Result};
use async_trait::async_trait;
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use hf_hub::{Repo, RepoType, api::sync::Api};
use tokenizers::Tokenizer;
use tokio_util::sync::CancellationToken;

use super::{EmbeddingProvider, EmbeddingResponse};

const MODEL_REPO: &str = "sentence-transformers/all-MiniLM-L6-v2";
const DIMENSIONS: usize = 384;

/// Local embedding provider using candle to run MiniLM-L6-v2 on CPU.
pub struct CandleEmbedding {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    cancel: CancellationToken,
}

impl CandleEmbedding {
    /// Load model weights and tokenizer. Checks `LOCAL_MODEL_DIR` first, then
    /// falls back to HuggingFace Hub download.
    ///
    /// `cancel` is the top-level shutdown token — stored on the provider so
    /// `embed()` can short-circuit between calls during shutdown.
    pub async fn load(cancel: CancellationToken) -> Result<Self> {
        // Blocking I/O — run in spawn_blocking to avoid stalling the tokio runtime.
        tokio::task::spawn_blocking(move || Self::load_sync(cancel))
            .await
            .context("join error")?
    }

    /// Resolve a model file, preferring the local directory over HF Hub.
    fn resolve_file(
        local_dir: &Option<std::path::PathBuf>,
        repo: &Option<hf_hub::api::sync::ApiRepo>,
        filename: &str,
    ) -> Result<std::path::PathBuf> {
        if let Some(dir) = local_dir {
            let path = dir.join(filename);
            if path.exists() {
                tracing::info!("Loading {} from local dir: {}", filename, path.display());
                return Ok(path);
            }
        }
        if let Some(repo) = repo {
            return repo
                .get(filename)
                .context(format!("fetch {filename} from HF Hub"));
        }
        anyhow::bail!("No local model dir and HF Hub unavailable for {filename}")
    }

    fn load_sync(cancel: CancellationToken) -> Result<Self> {
        let device = Device::Cpu;

        // Check for local model directory (env var or default relative path)
        let local_dir = std::env::var("LOCAL_MODEL_DIR")
            .ok()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                let default = std::path::PathBuf::from("models/all-MiniLM-L6-v2");
                if default.exists() {
                    Some(default)
                } else {
                    None
                }
            });

        // Try HF Hub as fallback (may fail on some networks)
        let repo = Api::new()
            .ok()
            .map(|api| api.repo(Repo::new(MODEL_REPO.into(), RepoType::Model)));

        if local_dir.is_none() && repo.is_none() {
            anyhow::bail!(
                "No local model dir found and HF Hub API failed. \
                 Set LOCAL_MODEL_DIR to the directory containing config.json, \
                 tokenizer.json, and model.safetensors."
            );
        }

        let config_path = Self::resolve_file(&local_dir, &repo, "config.json")?;
        let tokenizer_path = Self::resolve_file(&local_dir, &repo, "tokenizer.json")?;
        let weights_path = Self::resolve_file(&local_dir, &repo, "model.safetensors")?;

        // Load tokenizer
        let tokenizer = Tokenizer::from_file(tokenizer_path).map_err(|e| anyhow::anyhow!("{e}"))?;

        // Load model config + weights
        let config_str = std::fs::read_to_string(config_path)?;
        let config: BertConfig = serde_json::from_str(&config_str)?;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], candle_core::DType::F32, &device)?
        };
        let model = BertModel::load(vb, &config)?;

        Ok(Self {
            model,
            tokenizer,
            device,
            cancel,
        })
    }

    /// Perform mean-pooling over the last hidden state to produce a single vector.
    fn mean_pool(hidden: &Tensor, attention_mask: &Tensor) -> Result<Vec<f32>> {
        let mask = attention_mask
            .unsqueeze(2)?
            .to_dtype(candle_core::DType::F32)?;
        let masked = hidden.broadcast_mul(&mask)?;
        let summed = masked.sum(1)?;
        let counts = mask.sum(1)?;
        let pooled = summed.broadcast_div(&counts)?;
        let vec = pooled.squeeze(0)?.to_vec1::<f32>()?;
        Ok(vec)
    }
}

#[async_trait]
impl EmbeddingProvider for CandleEmbedding {
    fn provider_label(&self) -> &'static str {
        "local"
    }

    async fn embed(&self, text: &str) -> Result<EmbeddingResponse> {
        // C2 cooperative cancel: local CPU inference cannot be cancelled
        // mid-forward-pass, so we check the token at the top of each call.
        // Callers in shutdown-aware loops will see Err and exit naturally.
        if self.cancel.is_cancelled() {
            return Err(anyhow::anyhow!("shutdown: candle embed cancelled"));
        }

        // NOTE: Ideally this would use spawn_blocking to avoid blocking the
        // tokio runtime, but BertModel / Tensor are !Send so they cannot cross
        // an await boundary into a spawned task. CPU inference for MiniLM-L6-v2
        // on short texts completes in ~1-5 ms, so the blocking cost is
        // acceptable for single-call usage. For high-throughput scenarios
        // consider a dedicated inference thread with a channel-based API.

        // Tokenize — clamp to model max length (256 tokens for MiniLM)
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let ids = encoding.get_ids();
        let mask = encoding.get_attention_mask();

        let token_ids = Tensor::new(ids, &self.device)?.unsqueeze(0)?;
        let token_type_ids = token_ids.zeros_like()?;
        let attention_mask = Tensor::new(mask, &self.device)?.unsqueeze(0)?;

        let hidden = self
            .model
            .forward(&token_ids, &token_type_ids, Some(&attention_mask))?;

        let vector = Self::mean_pool(&hidden, &attention_mask)?;
        Ok(EmbeddingResponse {
            vector,
            tokens_used: 0,
            model: "sentence-transformers/all-MiniLM-L6-v2".into(),
        })
    }

    fn dimensions(&self) -> usize {
        DIMENSIONS
    }
}
