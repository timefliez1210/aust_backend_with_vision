use crate::VolumeError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone)]
pub struct VisionServiceClient {
    client: reqwest::Client,
    base_url: String,
    video_base_url: String,
    max_retries: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VisionServiceResponse {
    pub job_id: String,
    pub status: String,
    pub detected_items: Vec<VisionDetectedItem>,
    pub total_volume_m3: f64,
    pub confidence_score: f64,
    pub processing_time_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VisionDetectedItem {
    pub name: String,
    pub volume_m3: f64,
    pub dimensions: Option<VisionItemDimensions>,
    pub confidence: f64,
    pub seen_in_images: Vec<usize>,
    pub category: Option<String>,
    #[serde(default)]
    pub bbox: Option<Vec<f64>>,
    #[serde(default)]
    pub bbox_image_index: Option<usize>,
    #[serde(default)]
    pub crop_base64: Option<String>,
    #[serde(default)]
    pub german_name: Option<String>,
    #[serde(default)]
    pub re_value: Option<f64>,
    #[serde(default)]
    pub units: Option<u32>,
    #[serde(default)]
    pub volume_source: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VisionItemDimensions {
    pub length_m: f64,
    pub width_m: f64,
    pub height_m: f64,
}

impl VisionServiceClient {
    pub fn new(base_url: &str, video_base_url: Option<&str>, timeout_secs: u64, max_retries: u32) -> Result<Self, VolumeError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| VolumeError::ExternalService(format!("Failed to create HTTP client: {e}")))?;

        let base = base_url.trim_end_matches('/').to_string();
        let video = video_base_url
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_else(|| base.clone());

        Ok(Self {
            client,
            base_url: base,
            video_base_url: video,
            max_retries,
        })
    }

    /// Upload raw image bytes directly to the vision service as multipart form data.
    /// Used when the vision service doesn't have access to S3 (e.g. Modal deployment).
    pub async fn estimate_upload(
        &self,
        job_id: &str,
        images: &[(Vec<u8>, String)], // (data, mime_type)
    ) -> Result<VisionServiceResponse, VolumeError> {
        let url = format!("{}/estimate/upload", self.base_url);

        self.send_with_retry(&url, |client, url| {
            let mut form = reqwest::multipart::Form::new()
                .text("job_id", job_id.to_string());

            for (idx, (data, mime_type)) in images.iter().enumerate() {
                let ext = match mime_type.as_str() {
                    "image/png" => "png",
                    "image/webp" => "webp",
                    _ => "jpg",
                };
                let part = reqwest::multipart::Part::bytes(data.clone())
                    .file_name(format!("{idx}.{ext}"))
                    .mime_str(mime_type)
                    .unwrap_or_else(|_| {
                        reqwest::multipart::Part::bytes(data.clone())
                            .file_name(format!("{idx}.{ext}"))
                    });
                form = form.part("images", part);
            }

            client.post(url).multipart(form)
        }).await
    }

    /// Upload a video directly to the vision service for 3D volume estimation.
    /// Video processing takes 2-10 minutes with model swapping on GPU.
    pub async fn estimate_video(
        &self,
        job_id: &str,
        video_data: &[u8],
        mime: &str,
        max_keyframes: Option<u32>,
        detection_threshold: Option<f64>,
    ) -> Result<VisionServiceResponse, VolumeError> {
        let url = format!("{}/estimate/video", self.video_base_url);

        // Force HTTP/1.1 — HTTP/2 can stall on large multipart uploads due to
        // flow-control issues, causing reqwest's timeout to never fire.
        let video_client = reqwest::Client::builder()
            .http1_only()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(660))
            .build()
            .map_err(|e| {
                VolumeError::ExternalService(format!(
                    "Failed to create video HTTP client: {e}"
                ))
            })?;

        let ext = match mime {
            "video/quicktime" => "mov",
            "video/webm" => "webm",
            "video/x-matroska" => "mkv",
            _ => "mp4",
        };

        let mut form = reqwest::multipart::Form::new()
            .text("job_id", job_id.to_string());

        if let Some(kf) = max_keyframes {
            form = form.text("max_keyframes", kf.to_string());
        }
        if let Some(dt) = detection_threshold {
            form = form.text("detection_threshold", dt.to_string());
        }

        let part = reqwest::multipart::Part::bytes(video_data.to_vec())
            .file_name(format!("video.{ext}"))
            .mime_str(mime)
            .unwrap_or_else(|_| {
                reqwest::multipart::Part::bytes(video_data.to_vec())
                    .file_name(format!("video.{ext}"))
            });
        form = form.part("video", part);

        tracing::info!(%url, video_size = video_data.len(), "Sending video to vision service...");

        // Hard outer timeout as safety net (tokio-level, cannot be defeated by
        // stuck HTTP connections unlike reqwest's internal timeout)
        let result = tokio::time::timeout(
            Duration::from_secs(660),
            self.send_video_request(&video_client, &url, form),
        )
        .await
        .map_err(|_| {
            tracing::error!("Vision service video request timed out (660s hard limit)");
            VolumeError::ExternalService(
                "Vision service video request timed out after 660s".to_string(),
            )
        })?;

        result
    }

    async fn send_video_request(
        &self,
        client: &reqwest::Client,
        url: &str,
        form: reqwest::multipart::Form,
    ) -> Result<VisionServiceResponse, VolumeError> {
        let resp = client
            .post(url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "Vision service video send() failed");
                VolumeError::ExternalService(format!(
                    "Vision service video request failed: {e}"
                ))
            })?;

        let status = resp.status();
        tracing::info!(%status, "Vision service video response headers received");

        if status.is_success() {
            let body = resp.text().await.map_err(|e| {
                tracing::error!(error = %e, "Vision service video body read failed");
                VolumeError::ExternalService(format!(
                    "Failed to read vision service video response body: {e}"
                ))
            })?;
            tracing::info!(
                body_len = body.len(),
                body_preview = &body[..body.len().min(500)],
                "Vision service video raw response"
            );
            serde_json::from_str(&body).map_err(|e| {
                tracing::error!(
                    error = %e,
                    body_preview = &body[..body.len().min(1000)],
                    "Failed to deserialize vision service video response"
                );
                VolumeError::ExternalService(format!(
                    "Failed to parse vision service video response: {e}"
                ))
            })
        } else {
            let body = resp.text().await.unwrap_or_default();
            tracing::error!(%status, %body, "Vision service video returned error");
            Err(VolumeError::ExternalService(format!(
                "Vision service video returned {status}: {body}"
            )))
        }
    }

    async fn send_with_retry<F>(
        &self,
        url: &str,
        build_request: F,
    ) -> Result<VisionServiceResponse, VolumeError>
    where
        F: Fn(&reqwest::Client, &str) -> reqwest::RequestBuilder,
    {
        let mut last_err = None;

        for attempt in 0..=self.max_retries {
            if attempt > 0 {
                tracing::warn!(
                    attempt,
                    max_retries = self.max_retries,
                    "Retrying vision service request"
                );
                tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
            }

            match build_request(&self.client, url).send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        return resp.json().await.map_err(|e| {
                            VolumeError::ExternalService(format!(
                                "Failed to parse vision service response: {e}"
                            ))
                        });
                    }

                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();

                    if status.is_server_error() && attempt < self.max_retries {
                        last_err = Some(format!("Vision service returned {status}: {body}"));
                        continue;
                    }

                    return Err(VolumeError::ExternalService(format!(
                        "Vision service returned {status}: {body}"
                    )));
                }
                Err(e) => {
                    if attempt < self.max_retries {
                        last_err = Some(format!("Vision service request failed: {e}"));
                        continue;
                    }
                    return Err(VolumeError::ExternalService(format!(
                        "Vision service request failed after {} attempts: {e}",
                        attempt + 1
                    )));
                }
            }
        }

        Err(VolumeError::ExternalService(
            last_err.unwrap_or_else(|| "Vision service request failed".to_string()),
        ))
    }
}
