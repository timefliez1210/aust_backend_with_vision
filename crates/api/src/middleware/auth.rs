use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::Response,
};

#[derive(Clone)]
pub struct AuthLayer {
    _jwt_secret: String,
}

impl AuthLayer {
    pub fn new(jwt_secret: String) -> Self {
        Self {
            _jwt_secret: jwt_secret,
        }
    }
}

pub async fn auth_middleware(request: Request, next: Next) -> Result<Response, StatusCode> {
    // TODO: Implement JWT validation
    let _auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok());

    // For now, allow all requests
    Ok(next.run(request).await)
}
