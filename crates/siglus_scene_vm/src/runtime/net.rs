use anyhow::{anyhow, Context, Result};
use std::path::Path;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use std::process::Command;

#[derive(Debug, Default, Clone)]
pub struct TnmNet {
    pub last_target: Option<String>,
    pub last_status: Option<u16>,
    pub last_error: Option<String>,
}

impl TnmNet {
    fn clear_status(&mut self, target: &str) {
        self.last_target = Some(target.to_string());
        self.last_status = None;
        self.last_error = None;
    }

    fn set_error(&mut self, target: &str, err: &str) {
        self.last_target = Some(target.to_string());
        self.last_status = None;
        self.last_error = Some(err.to_string());
    }

    pub fn open_target(&mut self, target: &str) -> Result<()> {
        self.clear_status(target);

        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        {
            #[cfg(target_os = "macos")]
            let mut cmd = {
                let mut c = Command::new("open");
                c.arg(target);
                c
            };
            #[cfg(target_os = "linux")]
            let mut cmd = {
                let mut c = Command::new("xdg-open");
                c.arg(target);
                c
            };
            #[cfg(target_os = "windows")]
            let mut cmd = {
                let mut c = Command::new("cmd");
                c.args(["/C", "start", "", target]);
                c
            };

            let status = cmd
                .status()
                .with_context(|| format!("open external target {target}"))?;
            if status.success() {
                Ok(())
            } else {
                let msg = format!("external opener exited with status {status}");
                self.set_error(target, &msg);
                Err(anyhow!(msg))
            }
        }

        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            let window = web_sys::window().ok_or_else(|| anyhow!("browser window unavailable"))?;
            match window.open_with_url_and_target_and_features(target, "_blank", "noopener,noreferrer",
            ) {
                Ok(_) => Ok(()),
                Err(error) => {
                    let message = format!("external opener failed: {error:?}");
                    self.set_error(target, &message);
                    Err(anyhow!(message))
                }
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows", all(target_arch = "wasm32", target_os = "unknown"))))]
        {
            let msg = "external opener is not implemented on this platform";
            self.set_error(target, msg);
            log::error!("{}: {}", msg, target);
            Err(anyhow!(msg))
        }
    }

    pub fn open_file(&mut self, path: &Path) -> Result<()> {
        let target = path
            .to_str()
            .ok_or_else(|| anyhow!("non-utf8 file path"))?
            .to_string();
        self.open_target(&target)
    }

    pub fn open_url(&mut self, url: &str) -> Result<()> {
        self.open_target(url)
    }

    /// Browser HTTP must yield to the event loop; it cannot block the main
    /// thread waiting for fetch. Native callers may retain the synchronous API.
    pub async fn get_bytes_async(&mut self, url: &str) -> Result<Vec<u8>> {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        { self.fetch_bytes(url, "GET", None).await }
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        { self.get_bytes(url) }
    }

    pub async fn post_bytes_async(&mut self, url: &str, content_type: &str, body: &[u8],
    ) -> Result<Vec<u8>> {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        { self.fetch_bytes(url, "POST", Some((content_type, body))).await }
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        { self.post_bytes(url, content_type, body) }
    }

    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    async fn fetch_bytes(&mut self, url: &str, method: &str, body: Option<(&str, &[u8])>,
    ) -> Result<Vec<u8>> {
        use wasm_bindgen::JsCast;
        use wasm_bindgen_futures::JsFuture;
        self.clear_status(url);
        let result = async {
            let options = web_sys::RequestInit::new();
            options.set_method(method);
            options.set_mode(web_sys::RequestMode::Cors);
            if let Some((_, bytes)) = body {
                options.set_body(&js_sys::Uint8Array::from(bytes).into());
            }
            let request = web_sys::Request::new_with_str_and_init(url, &options)
                .map_err(|e| anyhow!("invalid HTTP request: {e:?}"))?;
            if let Some((content_type, _)) = body {
                request.headers().set("Content-Type", content_type)
                    .map_err(|e| anyhow!("invalid HTTP content type: {e:?}"))?;
            }
            let window = web_sys::window().ok_or_else(|| anyhow!("browser window unavailable"))?;
            let response: web_sys::Response = JsFuture::from(window.fetch_with_request(&request)).await
                .map_err(|e| anyhow!("{method} failed (network or CORS): {e:?}"))?
                .dyn_into().map_err(|_| anyhow!("fetch returned a non-response value"))?;
            self.last_status = Some(response.status());
            anyhow::ensure!(response.ok(), "HTTP {} {}", response.status(), response.status_text());
            let buffer = response.array_buffer().map_err(|e| anyhow!("HTTP response body: {e:?}"))?;
            let buffer = JsFuture::from(buffer).await.map_err(|e| anyhow!("reading HTTP body: {e:?}"))?;
            Ok(js_sys::Uint8Array::new(&buffer).to_vec())
        }.await;
        if let Err(error) = &result { self.last_error = Some(format!("{error:#}")); }
        result
    }

    pub fn get_bytes(&mut self, url: &str) -> Result<Vec<u8>> {
        self.clear_status(url);

        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            let msg = "synchronous HTTP cannot run on the browser main thread; use get_bytes_async";
            self.set_error(url, msg);
            log::error!("{}: {}", msg, url);
            return Err(anyhow!(msg));
        }

        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            let response = ureq::get(url)
                .call()
                .with_context(|| format!("GET {url}"))?;
            self.last_status = Some(response.status());
            let mut reader = response.into_reader();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut reader, &mut bytes)
                .with_context(|| format!("read response body from {url}"))?;
            Ok(bytes)
        }
    }

    pub fn post_bytes(&mut self, url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>> {
        self.clear_status(url);

        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            let msg = "synchronous HTTP cannot run on the browser main thread; use post_bytes_async";
            self.set_error(url, msg);
            log::error!("{}: {} content_type={}", msg, url, content_type);
            let _ = body;
            return Err(anyhow!(msg));
        }

        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            let response = ureq::post(url)
                .set("Content-Type", content_type)
                .send_bytes(body)
                .with_context(|| format!("POST {url}"))?;
            self.last_status = Some(response.status());
            let mut reader = response.into_reader();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut reader, &mut bytes)
                .with_context(|| format!("read response body from {url}"))?;
            Ok(bytes)
        }
    }
}
