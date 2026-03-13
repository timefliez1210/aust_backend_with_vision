use crate::VolumeError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// HTTP client for the Python ML vision service (photo + video endpoints).
///
/// **Caller**: `crates/api` — both public submission handlers and admin-triggered estimation.
/// **Why**: Abstracts the Modal (serverless GPU) HTTP API behind async submit/poll pattern
///          so the Rust side never blocks a connection for the 2-10 min pipeline duration.
#[derive(Clone)]
pub struct VisionServiceClient {
    client: reqwest::Client,
    base_url: String,
    video_base_url: String,
    max_retries: u32,
}

/// Immediate acknowledgement returned by the async submit endpoints.
///
/// **Why**: The submit endpoints return `{"job_id": "...", "status": "accepted"}` without
/// waiting for the pipeline to complete, so the Rust caller can poll separately.
#[derive(Debug, Deserialize)]
pub struct VisionSubmitResponse {
    /// Echo of the job_id sent in the request.
    pub job_id: String,
    /// Always `"accepted"` on a successful submit.
    pub status: String,
}

/// Polling response from `/estimate/status/{job_id}` and `/estimate/video/status/{job_id}`.
///
/// **Why**: Enables the Rust caller to drive the submit → poll loop without holding
/// an open HTTP connection to Modal.
#[derive(Debug, Deserialize)]
pub struct VisionJobStatus {
    /// `"processing"`, `"succeeded"`, or `"failed"`.
    pub status: String,
    /// Present only when `status == "succeeded"`.
    pub result: Option<VisionServiceResponse>,
    /// Present only when `status == "failed"`.
    pub error: Option<String>,
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

    /// Submit a photo job to the async endpoint and return immediately.
    ///
    /// **Caller**: `estimate_upload_async` — the first step of the async photo pipeline.
    /// **Why**: The Modal photo endpoint now accepts a job and starts processing in the
    ///          background, returning `{"job_id": ..., "status": "accepted"}` without
    ///          blocking the HTTP connection for the 5-10 s pipeline duration.
    ///
    /// # Parameters
    /// - `job_id` — UUID string that identifies the job; used to poll for status later
    /// - `images` — raw image bytes paired with their MIME types
    ///
    /// # Returns
    /// `VisionSubmitResponse` with `status = "accepted"` on success.
    ///
    /// # Errors
    /// Returns `VolumeError::ExternalService` if the HTTP request fails or Modal returns
    /// a non-2xx status (e.g., service not ready).
    pub async fn submit_upload(
        &self,
        job_id: &str,
        images: &[(Vec<u8>, String)],
    ) -> Result<VisionSubmitResponse, VolumeError> {
        let url = format!("{}/estimate/submit", self.base_url);

        let mut form = reqwest::multipart::Form::new().text("job_id", job_id.to_string());

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
                    reqwest::multipart::Part::bytes(data.clone()).file_name(format!("{idx}.{ext}"))
                });
            form = form.part("images", part);
        }

        let resp = self
            .client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                VolumeError::ExternalService(format!("Vision submit request failed: {e}"))
            })?;

        if resp.status().is_success() {
            return resp.json::<VisionSubmitResponse>().await.map_err(|e| {
                VolumeError::ExternalService(format!("Failed to parse submit response: {e}"))
            });
        }

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(VolumeError::ExternalService(format!(
            "Vision submit returned {status}: {body}"
        )))
    }

    /// Submit a video job to the async endpoint and return immediately.
    ///
    /// **Caller**: `estimate_video_async` — the first step of the async video pipeline.
    /// **Why**: Same reasoning as `submit_upload` — avoids holding a long-lived HTTP
    ///          connection while MASt3R reconstructs the 3D scene (2-10 min).
    ///
    /// # Parameters
    /// - `job_id` — UUID string for the job
    /// - `video_data` — raw video bytes
    /// - `mime` — MIME type, e.g. `"video/mp4"`
    /// - `max_keyframes` — optional keyframe cap; `None` uses the server default (60)
    /// - `detection_threshold` — optional DINO confidence threshold; `None` uses 0.3
    ///
    /// # Returns
    /// `VisionSubmitResponse` with `status = "accepted"` on success.
    ///
    /// # Errors
    /// Returns `VolumeError::ExternalService` on network or HTTP errors.
    pub async fn submit_video(
        &self,
        job_id: &str,
        video_data: &[u8],
        mime: &str,
        max_keyframes: Option<u32>,
        detection_threshold: Option<f64>,
    ) -> Result<VisionSubmitResponse, VolumeError> {
        let url = format!("{}/estimate/video/submit", self.video_base_url);

        // Force HTTP/1.1 — large multipart uploads can stall under HTTP/2 flow control.
        let video_client = reqwest::Client::builder()
            .http1_only()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| {
                VolumeError::ExternalService(format!("Failed to create video submit client: {e}"))
            })?;

        let ext = match mime {
            "video/quicktime" => "mov",
            "video/webm" => "webm",
            "video/x-matroska" => "mkv",
            _ => "mp4",
        };

        let mut form = reqwest::multipart::Form::new().text("job_id", job_id.to_string());

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

        let resp = video_client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                VolumeError::ExternalService(format!("Vision video submit request failed: {e}"))
            })?;

        if resp.status().is_success() {
            return resp.json::<VisionSubmitResponse>().await.map_err(|e| {
                VolumeError::ExternalService(format!("Failed to parse video submit response: {e}"))
            });
        }

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(VolumeError::ExternalService(format!(
            "Vision video submit returned {status}: {body}"
        )))
    }

    /// Poll `/estimate/status/{job_id}` and return the current job status.
    ///
    /// **Caller**: `estimate_upload_async` polling loop.
    /// **Why**: Small GET request that decouples job progress checks from the initial
    ///          submit so the backend can wait without holding a Modal container connection.
    ///
    /// # Parameters
    /// - `job_id` — the UUID returned by `submit_upload`
    ///
    /// # Returns
    /// `VisionJobStatus` with `status` of `"processing"`, `"succeeded"`, or `"failed"`.
    /// If the container restarted and the job is gone, returns `status = "not_found"`.
    ///
    /// # Errors
    /// Returns `VolumeError::ExternalService` on network errors or unexpected HTTP status.
    pub async fn poll_job_status(&self, job_id: &str) -> Result<VisionJobStatus, VolumeError> {
        let url = format!("{}/estimate/status/{}", self.base_url, job_id);
        let resp = self.client.get(&url).send().await.map_err(|e| {
            VolumeError::ExternalService(format!("Poll request failed: {e}"))
        })?;

        let http_status = resp.status();
        if http_status == reqwest::StatusCode::NOT_FOUND {
            return Ok(VisionJobStatus {
                status: "not_found".to_string(),
                result: None,
                error: None,
            });
        }
        if http_status.is_success() {
            return resp.json::<VisionJobStatus>().await.map_err(|e| {
                VolumeError::ExternalService(format!("Failed to parse poll response: {e}"))
            });
        }
        let body = resp.text().await.unwrap_or_default();
        Err(VolumeError::ExternalService(format!(
            "Poll returned {http_status}: {body}"
        )))
    }

    /// Poll `/estimate/video/status/{job_id}` and return the current video job status.
    ///
    /// **Caller**: `estimate_video_async` polling loop.
    /// **Why**: Same as `poll_job_status` but targets the video endpoint on `video_base_url`.
    ///
    /// # Parameters
    /// - `job_id` — the UUID returned by `submit_video`
    ///
    /// # Returns
    /// `VisionJobStatus` with `status` of `"processing"`, `"succeeded"`, `"failed"`,
    /// or `"not_found"` when the container restarted and lost the in-memory job.
    ///
    /// # Errors
    /// Returns `VolumeError::ExternalService` on network errors or unexpected HTTP status.
    pub async fn poll_video_job_status(
        &self,
        job_id: &str,
    ) -> Result<VisionJobStatus, VolumeError> {
        let url = format!("{}/estimate/video/status/{}", self.video_base_url, job_id);
        let resp = self.client.get(&url).send().await.map_err(|e| {
            VolumeError::ExternalService(format!("Video poll request failed: {e}"))
        })?;

        let http_status = resp.status();
        if http_status == reqwest::StatusCode::NOT_FOUND {
            return Ok(VisionJobStatus {
                status: "not_found".to_string(),
                result: None,
                error: None,
            });
        }
        if http_status.is_success() {
            return resp.json::<VisionJobStatus>().await.map_err(|e| {
                VolumeError::ExternalService(format!("Failed to parse video poll response: {e}"))
            });
        }
        let body = resp.text().await.unwrap_or_default();
        Err(VolumeError::ExternalService(format!(
            "Video poll returned {http_status}: {body}"
        )))
    }

    /// Submit a photo job and poll until it succeeds, fails, or exhausts retries.
    ///
    /// **Caller**: `crates/api/src/services/vision::try_vision_service_async`
    /// **Why**: Encapsulates the full async submit → poll loop so the API layer
    ///          only needs to call a single high-level method.
    ///
    /// # Parameters
    /// - `job_id` — UUID string for the job (should be unique per estimation attempt)
    /// - `images` — raw image bytes paired with their MIME types
    /// - `poll_interval` — how long to wait between status polls
    /// - `max_polls` — maximum number of poll attempts before giving up
    /// - `max_retries` — how many times to resubmit after a `failed` or `not_found` status
    ///
    /// # Returns
    /// The full `VisionServiceResponse` on success.
    ///
    /// # Errors
    /// Returns `VolumeError::ExternalService` when:
    /// - Submit fails and retries are exhausted
    /// - Job reports `failed` and retries are exhausted
    /// - `max_polls` polls complete without a terminal status
    pub async fn estimate_upload_async(
        &self,
        job_id: &str,
        images: &[(Vec<u8>, String)],
        poll_interval: Duration,
        max_polls: u32,
        max_retries: u32,
    ) -> Result<VisionServiceResponse, VolumeError> {
        let mut retries_used = 0u32;

        'retry: loop {
            // 1. Submit job
            match self.submit_upload(job_id, images).await {
                Ok(resp) => {
                    tracing::info!(%job_id, "Photo job submitted, status={}", resp.status);
                }
                Err(e) => {
                    if retries_used < max_retries {
                        retries_used += 1;
                        tracing::warn!(
                            %job_id,
                            attempt = retries_used,
                            "Photo submit failed, retrying in 5s: {e}"
                        );
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue 'retry;
                    }
                    return Err(e);
                }
            }

            // 2. Poll for result
            for poll_num in 1..=max_polls {
                tokio::time::sleep(poll_interval).await;

                let status = match self.poll_job_status(job_id).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(%job_id, poll_num, "Poll error (will continue): {e}");
                        continue; // transient network error — keep polling
                    }
                };

                match status.status.as_str() {
                    "succeeded" => {
                        tracing::info!(%job_id, poll_num, "Photo job completed successfully");
                        return status.result.ok_or_else(|| {
                            VolumeError::ExternalService(
                                "Status succeeded but result field is missing".into(),
                            )
                        });
                    }
                    "processing" => {
                        tracing::debug!(%job_id, poll_num, max_polls, "Photo job still processing");
                    }
                    "failed" | "not_found" => {
                        let reason = if status.status == "not_found" {
                            "Container restarted, job lost".to_string()
                        } else {
                            status
                                .error
                                .unwrap_or_else(|| "Unknown pipeline error".to_string())
                        };
                        if retries_used < max_retries {
                            retries_used += 1;
                            tracing::warn!(
                                %job_id,
                                attempt = retries_used,
                                "Photo job {}: {reason}, resubmitting in 5s",
                                status.status
                            );
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            continue 'retry;
                        }
                        return Err(VolumeError::ExternalService(format!(
                            "Photo vision job failed after {retries_used} retries: {reason}"
                        )));
                    }
                    other => {
                        return Err(VolumeError::ExternalService(format!(
                            "Unknown photo job status: {other}"
                        )));
                    }
                }
            }

            // max_polls exhausted without a terminal status
            return Err(VolumeError::ExternalService(format!(
                "Photo vision service did not complete within {max_polls} polls ({} min)",
                max_polls as u64 * poll_interval.as_secs() / 60
            )));
        }
    }

    /// Submit a video job and poll until it succeeds, fails, or exhausts retries.
    ///
    /// **Caller**: `crates/api/src/services/vision::try_vision_service_video_async`
    /// **Why**: Same as `estimate_upload_async` but targets the video pipeline which runs
    ///          MASt3R 3D reconstruction (2-10 min) — much longer than photos.
    ///
    /// # Parameters
    /// - `job_id` — UUID string for the job
    /// - `video_data` — raw video bytes
    /// - `mime` — MIME type, e.g. `"video/mp4"`
    /// - `max_keyframes` — optional keyframe cap passed to the pipeline
    /// - `detection_threshold` — optional DINO confidence threshold
    /// - `poll_interval` — wait between status polls
    /// - `max_polls` — maximum poll attempts before timeout error
    /// - `max_retries` — resubmit budget for `failed`/`not_found` responses
    ///
    /// # Returns
    /// The full `VisionServiceResponse` on success.
    ///
    /// # Errors
    /// Same conditions as `estimate_upload_async`.
    pub async fn estimate_video_async(
        &self,
        job_id: &str,
        video_data: &[u8],
        mime: &str,
        max_keyframes: Option<u32>,
        detection_threshold: Option<f64>,
        poll_interval: Duration,
        max_polls: u32,
        max_retries: u32,
    ) -> Result<VisionServiceResponse, VolumeError> {
        let mut retries_used = 0u32;

        'retry: loop {
            // 1. Submit job
            match self
                .submit_video(job_id, video_data, mime, max_keyframes, detection_threshold)
                .await
            {
                Ok(resp) => {
                    tracing::info!(%job_id, "Video job submitted, status={}", resp.status);
                }
                Err(e) => {
                    if retries_used < max_retries {
                        retries_used += 1;
                        tracing::warn!(
                            %job_id,
                            attempt = retries_used,
                            "Video submit failed, retrying in 5s: {e}"
                        );
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue 'retry;
                    }
                    return Err(e);
                }
            }

            // 2. Poll for result
            for poll_num in 1..=max_polls {
                tokio::time::sleep(poll_interval).await;

                let status = match self.poll_video_job_status(job_id).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(%job_id, poll_num, "Video poll error (will continue): {e}");
                        continue; // transient network error — keep polling
                    }
                };

                match status.status.as_str() {
                    "succeeded" => {
                        tracing::info!(%job_id, poll_num, "Video job completed successfully");
                        return status.result.ok_or_else(|| {
                            VolumeError::ExternalService(
                                "Video status succeeded but result field is missing".into(),
                            )
                        });
                    }
                    "processing" => {
                        tracing::debug!(%job_id, poll_num, max_polls, "Video job still processing");
                    }
                    "failed" | "not_found" => {
                        let reason = if status.status == "not_found" {
                            "Container restarted, job lost".to_string()
                        } else {
                            status
                                .error
                                .unwrap_or_else(|| "Unknown video pipeline error".to_string())
                        };
                        if retries_used < max_retries {
                            retries_used += 1;
                            tracing::warn!(
                                %job_id,
                                attempt = retries_used,
                                "Video job {}: {reason}, resubmitting in 5s",
                                status.status
                            );
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            continue 'retry;
                        }
                        return Err(VolumeError::ExternalService(format!(
                            "Video vision job failed after {retries_used} retries: {reason}"
                        )));
                    }
                    other => {
                        return Err(VolumeError::ExternalService(format!(
                            "Unknown video job status: {other}"
                        )));
                    }
                }
            }

            // max_polls exhausted without a terminal status
            return Err(VolumeError::ExternalService(format!(
                "Video vision service did not complete within {max_polls} polls ({} min)",
                max_polls as u64 * poll_interval.as_secs() / 60
            )));
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
