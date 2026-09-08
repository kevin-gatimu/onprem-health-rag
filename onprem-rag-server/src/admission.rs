//! Bounded admission control for expensive local inference, retrieval, and ingestion work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;

use crate::config::Config;
use crate::error::{AppError, AppResult};

pub struct AdmissionControl {
    generation: Arc<Semaphore>,
    retrieval: Arc<Semaphore>,
    ingestion: Arc<Semaphore>,
    ingestion_by_source: Mutex<HashMap<String, Weak<Semaphore>>>,
    per_source_ingestion: usize,
    wait: Duration,
    /// How long a chat request waits for its turn to generate.
    ///
    /// Much longer than `wait`, because generation is deliberately capped at one at a
    /// time: ONNX Runtime's WebGPU execution provider corrupts its own device state when
    /// two generations overlap, which kills whichever process is hosting the model. So a
    /// second asker is not overload to shed — it is a normal caller that must queue.
    /// Rejecting it after 2 s would turn "wait your turn" into "your question failed".
    generation_wait: Duration,
}

pub struct IngestPermit {
    _global: OwnedSemaphorePermit,
    _source: OwnedSemaphorePermit,
}

impl AdmissionControl {
    pub fn new(config: &Config) -> Self {
        Self {
            generation: Arc::new(Semaphore::new(config.max_active_generations.max(1))),
            retrieval: Arc::new(Semaphore::new(config.max_active_retrievals.max(1))),
            ingestion: Arc::new(Semaphore::new(config.max_active_ingestions.max(1))),
            ingestion_by_source: Mutex::new(HashMap::new()),
            per_source_ingestion: config.max_ingestions_per_source.max(1),
            wait: Duration::from_millis(config.admission_timeout_ms.max(1)),
            generation_wait: Duration::from_millis(config.generation_queue_timeout_ms.max(1)),
        }
    }

    pub fn available_capacity(&self) -> (usize, usize, usize) {
        (
            self.generation.available_permits(),
            self.retrieval.available_permits(),
            self.ingestion.available_permits(),
        )
    }

    pub async fn generation(&self) -> AppResult<OwnedSemaphorePermit> {
        self.acquire_within(
            self.generation.clone(),
            "generation capacity is busy",
            self.generation_wait,
        )
        .await
    }

    pub async fn retrieval(&self) -> AppResult<OwnedSemaphorePermit> {
        self.acquire(self.retrieval.clone(), "retrieval capacity is busy")
            .await
    }

    pub async fn ingestion(&self, source_id: &str) -> AppResult<IngestPermit> {
        let source = {
            let mut sources = self.ingestion_by_source.lock().map_err(|_| {
                AppError::Internal("ingestion admission lock is unavailable".into())
            })?;
            sources.retain(|_, entry| entry.strong_count() > 0);
            if let Some(semaphore) = sources.get(source_id).and_then(Weak::upgrade) {
                semaphore
            } else {
                let semaphore = Arc::new(Semaphore::new(self.per_source_ingestion));
                sources.insert(source_id.to_string(), Arc::downgrade(&semaphore));
                semaphore
            }
        };
        let source = self
            .acquire(
                source,
                "this source already has the maximum active ingestions",
            )
            .await?;
        let global = self
            .acquire(self.ingestion.clone(), "ingestion capacity is busy")
            .await?;
        Ok(IngestPermit {
            _global: global,
            _source: source,
        })
    }

    async fn acquire(
        &self,
        semaphore: Arc<Semaphore>,
        message: &'static str,
    ) -> AppResult<OwnedSemaphorePermit> {
        self.acquire_within(semaphore, message, self.wait).await
    }

    async fn acquire_within(
        &self,
        semaphore: Arc<Semaphore>,
        message: &'static str,
        wait: Duration,
    ) -> AppResult<OwnedSemaphorePermit> {
        timeout(wait, semaphore.acquire_owned())
            .await
            .map_err(|_| {
                AppError::TooManyRequests(format!(
                    "{message}; retry after {} ms",
                    wait.as_millis()
                ))
            })?
            .map_err(|_| AppError::Unavailable("admission control is shutting down".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admission(wait_ms: u64) -> AdmissionControl {
        AdmissionControl {
            generation: Arc::new(Semaphore::new(1)),
            retrieval: Arc::new(Semaphore::new(1)),
            ingestion: Arc::new(Semaphore::new(2)),
            ingestion_by_source: Mutex::new(HashMap::new()),
            per_source_ingestion: 1,
            wait: Duration::from_millis(wait_ms),
            generation_wait: Duration::from_millis(wait_ms),
        }
    }

    #[tokio::test]
    async fn times_out_when_generation_capacity_is_held() {
        let admission = admission(5);
        let _permit = admission.generation().await.unwrap();
        assert!(matches!(
            admission.generation().await,
            Err(AppError::TooManyRequests(_))
        ));
    }

    #[tokio::test]
    async fn releases_generation_capacity_with_permit() {
        let admission = admission(50);
        let permit = admission.generation().await.unwrap();
        drop(permit);
        assert!(admission.generation().await.is_ok());
    }

    #[tokio::test]
    async fn limits_ingestion_per_source_independently() {
        let admission = admission(5);
        let _first = admission.ingestion("source-a").await.unwrap();
        assert!(matches!(
            admission.ingestion("source-a").await,
            Err(AppError::TooManyRequests(_))
        ));
        assert!(admission.ingestion("source-b").await.is_ok());
    }
}
