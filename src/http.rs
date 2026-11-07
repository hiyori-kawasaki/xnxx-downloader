use reqwest::{Client, Response};

use crate::error::{Error, Result};

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";

/// Cliente HTTP compartido (pool de conexiones reutilizable, clonable barato).
#[derive(Clone)]
pub struct HttpClient {
    inner: Client,
}

impl HttpClient {
    pub fn new() -> Result<Self> {
        let inner = Client::builder()
            .user_agent(USER_AGENT)
            .pool_max_idle_per_host(32)
            .tcp_nodelay(true)
            .build()?;
        Ok(Self { inner })
    }

    /// GET sin validar el status (el llamador decide, p. ej. para reintentos).
    pub async fn try_get(&self, url: &str) -> reqwest::Result<Response> {
        self.inner.get(url).send().await
    }

    /// GET que falla si el status no es 2xx.
    pub async fn get(&self, url: &str) -> Result<Response> {
        let response = self.try_get(url).await?;
        let status = response.status();
        if status.is_success() {
            Ok(response)
        } else {
            Err(Error::HttpStatus {
                status: status.as_u16(),
                url: url.to_owned(),
            })
        }
    }

    pub async fn get_text(&self, url: &str) -> Result<String> {
        Ok(self.get(url).await?.text().await?)
    }
}
