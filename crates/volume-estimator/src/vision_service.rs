use crate::VolumeError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone)]
pub struct VisionServiceClient {
    client: reqwest::Client,
    base_url: String,
    max_retries: u32,
}

#[derive(Debug, Serialize)]
pub struct VisionServiceRequest {
    pub job_id: String,
    pub s3_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<VisionServiceOptions>,
}

#[derive(Debug, Serialize)]
pub struct VisionServiceOptions {
    pub detection_threshold: Option<f64>,
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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VisionItemDimensions {
    pub length_m: f64,
    pub width_m: f64,
    pub height_m: f64,
}

#[derive(Debug, Deserialize)]
struct ReadyResponse {
    status: String,
}

impl VisionServiceClient {
    pub fn new(base_url: &str, timeout_secs: u64, max_retries: u32) -> Result<Self, VolumeError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| VolumeError::ExternalService(format!("Failed to create HTTP client: {e}")))?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            max_retries,
        })
    }

    pub async fn check_ready(&self) -> Result<bool, VolumeError> {
        let url = format!("{}/ready", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| VolumeError::ExternalService(format!("Vision service unreachable: {e}")))?;

        if resp.status().is_success() {
            let body: ReadyResponse = resp
                .json()
                .await
                .map_err(|e| VolumeError::ExternalService(format!("Invalid ready response: {e}")))?;
            Ok(body.status == "ready")
        } else {
            Ok(false)
        }
    }

    pub async fn estimate_images(
        &self,
        request: &VisionServiceRequest,
    ) -> Result<VisionServiceResponse, VolumeError> {
        let url = format!("{}/estimate/images", self.base_url);
        self.send_with_retry(&url, |client, url| {
            client.post(url).json(request)
        }).await
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
        let url = format!("{}/estimate/video", self.base_url);

        // Build a client with extended timeout for video processing (600s)
        let video_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
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

        // No retry for video — processing is too long to restart
        let resp = video_client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                VolumeError::ExternalService(format!(
                    "Vision service video request failed: {e}"
                ))
            })?;

        if resp.status().is_success() {
            resp.json().await.map_err(|e| {
                VolumeError::ExternalService(format!(
                    "Failed to parse vision service video response: {e}"
                ))
            })
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
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
