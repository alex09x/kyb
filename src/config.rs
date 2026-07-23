use std::path::PathBuf;

#[derive(Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub index_dir: PathBuf,
    pub audit_path: PathBuf,
    /// Directory with model.onnx + tokenizer.json. Absent = lexical-only.
    pub model_dir: PathBuf,
    pub addr: String,
}

impl Config {
    /// Everything has defaults; .env holds optional overrides.
    /// No auth on purpose: the service listens on 127.0.0.1 only.
    pub fn from_env() -> Config {
        dotenvy::dotenv().ok();
        Config {
            data_dir: std::env::var("KYB_DATA").unwrap_or_else(|_| "./kyb-data".into()).into(),
            index_dir: std::env::var("KYB_INDEX").unwrap_or_else(|_| "./index".into()).into(),
            audit_path: std::env::var("KYB_AUDIT").unwrap_or_else(|_| "./audit.jsonl".into()).into(),
            model_dir: std::env::var("KYB_MODEL").unwrap_or_else(|_| "./model".into()).into(),
            addr: std::env::var("KYB_ADDR").unwrap_or_else(|_| "127.0.0.1:9310".into()),
        }
    }
}
