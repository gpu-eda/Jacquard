use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunParams {
    pub master_seed: u64,
}

impl RunParams {
    pub fn generate() -> Self {
        Self {
            master_seed: rand::random(),
        }
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        serde_json::from_str(&contents).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }

    pub fn load_or_generate(path: &Path) -> std::io::Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            let params = Self::generate();
            params.write(path)?;
            Ok(params)
        }
    }

    pub fn domain_seed(&self, domain_name: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.master_seed.hash(&mut hasher);
        domain_name.hash(&mut hasher);
        hasher.finish()
    }
}
