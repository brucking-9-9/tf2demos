//! config (stub)
use std::path::Path;
use anyhow::Result;
pub struct Config;
impl Config { pub fn load(_p: Option<&Path>) -> Result<Self> { Ok(Config) } }
