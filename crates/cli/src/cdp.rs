//! A small synchronous Chrome DevTools Protocol client: one browser connection, flat
//! sessions, just the handful of calls this tool needs.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

pub struct Event {
    pub session: Option<String>,
    pub method: String,
    pub params: Value,
}

pub struct Cdp {
    ws: WebSocket<TcpStream>,
    next_id: u64,
    events: VecDeque<Event>,
    pub timeout: Duration,
}

impl Cdp {
    pub fn connect(ws_url: &str) -> Result<Cdp> {
        let url = url::Url::parse(ws_url).context("Chrome reported an unusable DevTools address")?;
        let host = url.host_str().ok_or_else(|| anyhow!("DevTools address has no host"))?;
        let port = url.port().ok_or_else(|| anyhow!("DevTools address has no port"))?;
        let stream = TcpStream::connect((host, port)).context("cannot reach Chrome's DevTools port")?;
        stream.set_nodelay(true).ok();
        // Snapshots of large pages are tens of megabytes in one message.
        let config = WebSocketConfig::default().max_message_size(None).max_frame_size(None);
        let (ws, _) = tungstenite::client::client_with_config(ws_url, stream, Some(config))
            .map_err(|e| anyhow!("DevTools handshake failed: {e}"))?;
        ws.get_ref().set_read_timeout(Some(Duration::from_millis(200)))?;
        Ok(Cdp { ws, next_id: 1, events: VecDeque::new(), timeout: Duration::from_secs(120) })
    }

    /// Next message, or `None` once the deadline has passed.
    fn read(&mut self, deadline: Instant) -> Result<Option<Value>> {
        loop {
            match self.ws.read() {
                Ok(Message::Text(t)) => return Ok(Some(serde_json::from_str(t.as_str()).context("Chrome sent malformed JSON")?)),
                Ok(Message::Close(_)) => bail!("Chrome closed the DevTools connection"),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                }
                Err(e) => bail!("DevTools connection failed: {e}"),
            }
        }
    }

    fn stash(&mut self, mut v: Value) {
        let Some(method) = v.get("method").and_then(Value::as_str).map(str::to_string) else { return };
        let session = v.get("sessionId").and_then(Value::as_str).map(str::to_string);
        let params = v.get_mut("params").map(Value::take).unwrap_or(Value::Null);
        self.events.push_back(Event { session, method, params });
        if self.events.len() > 20_000 {
            self.events.pop_front();
        }
    }

    pub fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.ws.send(Message::text(msg.to_string())).with_context(|| format!("sending {method}"))?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let Some(mut v) = self.read(deadline)? else { bail!("{method} timed out after {}s", self.timeout.as_secs()) };
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = v.get("error") {
                    bail!("{method} failed: {}", err.get("message").and_then(Value::as_str).unwrap_or("unknown error"));
                }
                return Ok(v.get_mut("result").map(Value::take).unwrap_or(Value::Null));
            }
            self.stash(v);
        }
    }

    /// Wait for an event that satisfies `pred`. Returns `None` on timeout.
    pub fn wait_event(&mut self, session: Option<&str>, method: &str, timeout: Duration, pred: &dyn Fn(&Value) -> bool) -> Result<Option<Value>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pos) = self.events.iter().position(|e| e.method == method && e.session.as_deref() == session && pred(&e.params)) {
                return Ok(self.events.remove(pos).map(|e| e.params));
            }
            match self.read(deadline)? {
                Some(v) => self.stash(v),
                None => return Ok(None),
            }
        }
    }

    pub fn take_events(&mut self, session: Option<&str>, method: &str) -> Vec<Value> {
        let mut out = Vec::new();
        let mut rest = VecDeque::with_capacity(self.events.len());
        for e in self.events.drain(..) {
            if e.method == method && e.session.as_deref() == session {
                out.push(e.params);
            } else {
                rest.push_back(e);
            }
        }
        self.events = rest;
        out
    }
}

/// One tab.
#[derive(Clone)]
pub struct Page {
    cdp: Rc<RefCell<Cdp>>,
    session: String,
}

impl Page {
    pub fn open(cdp: &Rc<RefCell<Cdp>>) -> Result<Page> {
        let target = cdp.borrow_mut().call(None, "Target.createTarget", json!({ "url": "about:blank" }))?;
        let tid = target["targetId"].as_str().ok_or_else(|| anyhow!("Chrome did not create a tab"))?.to_string();
        let att = cdp.borrow_mut().call(None, "Target.attachToTarget", json!({ "targetId": tid, "flatten": true }))?;
        let session = att["sessionId"].as_str().ok_or_else(|| anyhow!("Chrome did not attach to the tab"))?.to_string();
        let page = Page { cdp: cdp.clone(), session };
        page.call("Page.enable", json!({}))?;
        page.call("Page.setLifecycleEventsEnabled", json!({ "enabled": true }))?;
        Ok(page)
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.cdp.borrow_mut().call(Some(&self.session), method, params)
    }

    pub fn take_events(&self, method: &str) -> Vec<Value> {
        self.cdp.borrow_mut().take_events(Some(&self.session), method)
    }

    /// Evaluate an expression (promises are awaited) and return its JSON value.
    pub fn eval(&self, expr: &str) -> Result<Value> {
        let mut r = self.call("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true, "awaitPromise": true }))?;
        if let Some(ex) = r.get("exceptionDetails") {
            let msg = ex["exception"]["description"].as_str().or_else(|| ex["text"].as_str()).unwrap_or("script error");
            bail!("page script failed: {msg}");
        }
        Ok(r.get_mut("result").and_then(|x| x.get_mut("value")).map(Value::take).unwrap_or(Value::Null))
    }

    pub fn eval_string(&self, expr: &str) -> Result<String> {
        match self.eval(expr)? {
            Value::String(s) => Ok(s),
            other => bail!("page script returned {} where a string was expected", other),
        }
    }

    /// Navigate and wait for the load event, then briefly for the network to go quiet.
    /// Returns false when the page never finished loading (it is used as it stands).
    pub fn navigate(&self, url: &str, timeout: Duration, wait_idle: bool) -> Result<bool> {
        self.take_events("Page.loadEventFired");
        self.take_events("Page.lifecycleEvent");
        let r = self.call("Page.navigate", json!({ "url": url }))?;
        if let Some(err) = r.get("errorText").and_then(Value::as_str) {
            if !err.is_empty() {
                bail!("could not load {url}: {err}");
            }
        }
        let s = Some(self.session.as_str());
        let loaded = self.cdp.borrow_mut().wait_event(s, "Page.loadEventFired", timeout, &|_| true)?.is_some();
        if loaded && wait_idle {
            let idle = |p: &Value| p["name"] == "networkIdle";
            self.cdp.borrow_mut().wait_event(s, "Page.lifecycleEvent", Duration::from_secs(3), &idle)?;
        }
        Ok(loaded)
    }

    pub fn set_viewport(&self, w: u32, h: u32, dpr: f64) -> Result<()> {
        self.call("Emulation.setDeviceMetricsOverride", json!({ "width": w, "height": h, "deviceScaleFactor": dpr, "mobile": false }))?;
        Ok(())
    }

    /// PNG of a region given in document px.
    pub fn screenshot(&self, clip: [f64; 4], beyond_viewport: bool) -> Result<Vec<u8>> {
        let r = self.call(
            "Page.captureScreenshot",
            json!({
                "format": "png",
                "captureBeyondViewport": beyond_viewport,
                "clip": { "x": clip[0], "y": clip[1], "width": clip[2], "height": clip[3], "scale": 1 },
            }),
        )?;
        let data = r["data"].as_str().ok_or_else(|| anyhow!("Chrome returned no screenshot"))?;
        base64::engine::general_purpose::STANDARD.decode(data).context("screenshot was not valid base64")
    }
}
