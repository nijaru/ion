use async_trait::async_trait;
use ort::session::Session;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokenizers::Tokenizer;
use tokio::sync::Mutex;
use tracing::info;

#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("API error: {0}")]
    Api(String),
    #[error("Network error: {0}")]
    Network(String),
    #[error("Config error: {0}")]
    Config(String),
    #[error("Inference error: {0}")]
    Inference(String),
}

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Get the dimension of the embeddings produced by this provider.
    fn dimension(&self) -> usize;

    /// Embed a single string.
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;

    /// Embed multiple strings in a batch.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError>;
}

pub struct SnowflakeArcticProvider {
    session: Arc<Mutex<Session>>,
    tokenizer: Tokenizer,
    dimension: usize,
}

impl SnowflakeArcticProvider {
    /// Initialize the provider, downloading the model if it's missing.
    pub async fn load(model_dir: PathBuf) -> Result<Self, EmbeddingError> {
        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !model_path.exists() || !tokenizer_path.exists() {
            std::fs::create_dir_all(&model_dir).map_err(|e| {
                EmbeddingError::Config(format!("Failed to create model dir: {}", e))
            })?;

            info!("Downloading Snowflake Arctic model to {:?}...", model_dir);

            let client = reqwest::Client::new();

            // Download tokenizer
            let tokenizer_url = "https://huggingface.co/Snowflake/snowflake-arctic-embed-s/resolve/main/tokenizer.json";
            let resp = client
                .get(tokenizer_url)
                .send()
                .await
                .map_err(|e| EmbeddingError::Network(e.to_string()))?;
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| EmbeddingError::Network(e.to_string()))?;
            std::fs::write(&tokenizer_path, bytes)
                .map_err(|e| EmbeddingError::Config(format!("Failed to save tokenizer: {}", e)))?;

            // Download ONNX model (from the keisuke-miyako repo which has the standard ONNX layout)
            let model_url = "https://huggingface.co/Snowflake/snowflake-arctic-embed-s/resolve/main/onnx/model.onnx";
            let resp = client
                .get(model_url)
                .send()
                .await
                .map_err(|e| EmbeddingError::Network(e.to_string()))?;

            let bytes = resp
                .bytes()
                .await
                .map_err(|e| EmbeddingError::Network(e.to_string()))?;
            std::fs::write(&model_path, bytes)
                .map_err(|e| EmbeddingError::Config(format!("Failed to save model: {}", e)))?;

            info!("Model download complete.");
        }

        let mut session_builder =
            Session::builder().map_err(|e: ort::Error| EmbeddingError::Config(e.to_string()))?;

        // Enable hardware acceleration
        #[cfg(target_os = "macos")]
        {
            // Use CoreML for Apple Silicon acceleration
            session_builder = session_builder
                .with_execution_providers([
                    ort::execution_providers::CoreMLExecutionProvider::default().build(),
                ])
                .map_err(|e: ort::Error| EmbeddingError::Config(e.to_string()))?;
        }

        #[cfg(target_os = "linux")]
        {
            // Try CUDA first, then ROCm for AMD GPUs
            if let Ok(cuda) = ort::execution_providers::CUDAExecutionProvider::default().build() {
                session_builder = session_builder
                    .with_execution_providers([cuda])
                    .map_err(|e: ort::Error| EmbeddingError::Config(e.to_string()))?;
            } else if let Ok(rocm) =
                ort::execution_providers::ROCmExecutionProvider::default().build()
            {
                session_builder = session_builder
                    .with_execution_providers([rocm])
                    .map_err(|e: ort::Error| EmbeddingError::Config(e.to_string()))?;
            }
        }

        #[cfg(target_os = "windows")]
        {
            // Use DirectML for Windows (NVIDIA/AMD/Intel)
            session_builder = session_builder
                .with_execution_providers([
                    ort::execution_providers::DirectMLExecutionProvider::default().build(),
                ])
                .map_err(|e: ort::Error| EmbeddingError::Config(e.to_string()))?;
        }

        let session = session_builder
            .commit_from_file(model_path)
            .map_err(|e: ort::Error| EmbeddingError::Config(e.to_string()))?;

        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| EmbeddingError::Config(e.to_string()))?;

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer,
            dimension: 384,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for SnowflakeArcticProvider {
    fn dimension(&self) -> usize {
        self.dimension
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let results = self.embed_batch(&[text.to_string()]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::Inference("No embedding produced".to_string()))
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| EmbeddingError::Inference(e.to_string()))?;

        let batch_size = texts.len();
        let max_len = encodings.iter().map(|e| e.len()).max().unwrap_or(0);

        if max_len == 0 {
            return Ok(vec![vec![0.0; self.dimension]; batch_size]);
        }

        // Initialize padded tensors
        let mut ids_array = ndarray::Array2::<i64>::zeros((batch_size, max_len));
        let mut mask_array = ndarray::Array2::<i64>::zeros((batch_size, max_len));
        let mut type_ids_array = ndarray::Array2::<i64>::zeros((batch_size, max_len));

        for (i, encoding) in encodings.iter().enumerate() {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();
            let type_ids = encoding.get_type_ids();
            let len = ids.len();

            for j in 0..len {
                ids_array[[i, j]] = ids[j] as i64;
                mask_array[[i, j]] = mask[j] as i64;
                type_ids_array[[i, j]] = type_ids[j] as i64;
            }
        }

        let input_ids_val = ort::value::Value::from_array(ids_array)
            .map_err(|e: ort::Error| EmbeddingError::Inference(e.to_string()))?;
        let attention_mask_val = ort::value::Value::from_array(mask_array)
            .map_err(|e: ort::Error| EmbeddingError::Inference(e.to_string()))?;
        let token_type_ids_val = ort::value::Value::from_array(type_ids_array)
            .map_err(|e: ort::Error| EmbeddingError::Inference(e.to_string()))?;

        let inputs = ort::inputs![
            "input_ids" => input_ids_val,
            "attention_mask" => attention_mask_val,
            "token_type_ids" => token_type_ids_val,
        ];

        let mut session = self.session.lock().await;
        let outputs = session
            .run(inputs)
            .map_err(|e: ort::Error| EmbeddingError::Inference(e.to_string()))?;

        let output_tensor = outputs["last_hidden_state"]
            .try_extract_array::<f32>()
            .map_err(|e: ort::Error| EmbeddingError::Inference(e.to_string()))?;

        // Mean pooling: [batch, seq_len, dim] -> [batch, dim]
        let view = output_tensor.view();
        let actual_dim = view.shape()[2];

        if actual_dim != self.dimension {
            return Err(EmbeddingError::Inference(format!(
                "Model output dimension {} does not match expected dimension {}",
                actual_dim, self.dimension
            )));
        }

        let mut all_embeddings = Vec::with_capacity(batch_size);

        for b in 0..batch_size {
            let mut mean_embedding = vec![0.0f32; self.dimension];
            let mut valid_tokens = 0.0f32;
            let current_mask = encodings[b].get_attention_mask();

            for t in 0..max_len {
                // Only pool non-padded tokens using the attention mask
                if t < current_mask.len() && current_mask[t] == 1 {
                    for d in 0..self.dimension {
                        mean_embedding[d] += view[[b, t, d]];
                    }
                    valid_tokens += 1.0;
                }
            }

            if valid_tokens > 0.0 {
                for d in 0..self.dimension {
                    mean_embedding[d] /= valid_tokens;
                }

                // L2 Normalization
                let norm = mean_embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 1e-6 {
                    for x in &mut mean_embedding {
                        *x /= norm;
                    }
                }
            }

            all_embeddings.push(mean_embedding);
        }

        Ok(all_embeddings)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIConfig {
    pub api_key: String,
    pub model: String,
    pub dimension: usize,
}

pub struct OpenAIProvider {
    client: reqwest::Client,
    config: OpenAIConfig,
}

impl OpenAIProvider {
    pub fn new(config: OpenAIConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
        }
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAIProvider {
    fn dimension(&self) -> usize {
        self.config.dimension
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let results = self.embed_batch(&[text.to_string()]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::Api("No embedding returned from OpenAI".to_string()))
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        #[derive(Serialize)]
        struct Request {
            input: Vec<String>,
            model: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            dimensions: Option<usize>,
        }

        #[derive(Deserialize)]
        struct Response {
            data: Vec<EmbeddingData>,
        }

        #[derive(Deserialize)]
        struct EmbeddingData {
            embedding: Vec<f32>,
        }

        let req = Request {
            input: texts.to_vec(),
            model: self.config.model.clone(),
            dimensions: Some(self.config.dimension),
        };

        let resp: Response = self
            .client
            .post("https://api.openai.com/v1/embeddings")
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .json(&req)
            .send()
            .await
            .map_err(|e| EmbeddingError::Network(e.to_string()))?
            .json()
            .await
            .map_err(|e| EmbeddingError::Api(e.to_string()))?;

        Ok(resp.data.into_iter().map(|d| d.embedding).collect())
    }
}

#[cfg(test)]
pub struct MockEmbeddingProvider {
    pub dimension: usize,
}

#[cfg(test)]
#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    fn dimension(&self) -> usize {
        self.dimension
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(vec![0.1; self.dimension])
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(vec![vec![0.1; self.dimension]; texts.len()])
    }
}
