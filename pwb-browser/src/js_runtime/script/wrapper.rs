//! Injected JS `document`/`pwb`/Web API wrapper source.

use super::super::dom_utils::js_escape;

pub(super) fn document_wrapper(title: &str) -> String {
    format!(
        r##"
            function makeEl(nodeId) {{
                return {{
                    id: __pwb_getElementId(nodeId),
                    nodeId: nodeId,
                    text: __pwb_getText(nodeId),
                    getText: function() {{ return __pwb_getText(nodeId); }},
                    setText: function(t) {{ __pwb_setText(nodeId, t); this.text = t; }},
                    html: __pwb_getText(nodeId),
                    getHtml: function() {{ return __pwb_getText(nodeId); }},
                    setHtml: function(h) {{ __pwb_setText(nodeId, h); this.html = h; }},
                    value: __pwb_getValue(nodeId),
                    getValue: function() {{ return __pwb_getValue(nodeId); }},
                    setValue: function(v) {{ __pwb_setValue(nodeId, String(v)); this.value = String(v); }},
                    focus: function() {{ __pwb_focusInput(nodeId); }},
                    blur: function() {{ __pwb_blurInput(); }},
                    getStyle: function(p) {{ return __pwb_getStyle(nodeId, p); }},
                    setStyle: function(p, v) {{ __pwb_setStyle(nodeId, p, v); }},
                    onClick: function(handler) {{ __pwb_registerEventHandler(nodeId, "click", String(handler)); }},
                    addEventHandler: function(evt, handler) {{ __pwb_registerEventHandler(nodeId, evt, String(handler)); }},
                    setAttribute: function(k, v) {{ __pwb_setAttribute(nodeId, String(k), String(v)); }},
                    getAttribute: function(k) {{ return __pwb_getAttribute(nodeId, String(k)); }},
                    removeAttribute: function(k) {{ __pwb_removeAttribute(nodeId, String(k)); }},
                    appendChild: function(child) {{ if (child && child.nodeId) __pwb_appendChild(nodeId, child.nodeId); return child; }},
                    removeChild: function(child) {{ if (child && child.nodeId) __pwb_removeChild(nodeId, child.nodeId); return child; }},
                    remove: function() {{ __pwb_removeFromParent(nodeId); }}
                }};
            }}

            globalThis.document = {{
                title: "{title}",
                selector: function(sel) {{
                    var targetId = __pwb_querySelector(sel);
                    if (targetId && targetId.length > 0) {{ return makeEl(targetId); }}
                    return null;
                }},
                getElementById: function(id) {{
                    return this.selector("#" + id);
                }},
                createElement: function(tag) {{
                    var id = __pwb_createElement(String(tag));
                    return makeEl(String(id));
                }},
                body: makeEl(String(__pwb_documentRoot()))
            }};

            // PlainWeb module system: components, runtime styles, and dynamic
            // script loading via the bundled named-module map.
            globalThis.pwb = {{
                components: function() {{
                    var s = __pwb_componentNames();
                    return s.length ? s.split(",") : [];
                }},
                createComponent: function(name, props, parentSelector) {{
                    var id = __pwb_createComponent(name, JSON.stringify(props || {{}}), parentSelector || "");
                    return id && id.length > 0 ? makeEl(id) : null;
                }},
                loadStyle: function(css) {{ return __pwb_loadStyle(css); }},
                import: function(name) {{
                    var src = __pwb_importModule(name);
                    if (src) {{ (0, eval)(src); return true; }}
                    return false;
                }},
                hotload: function(name) {{
                    var src = __pwb_hotloadModule(name);
                    if (src) {{ (0, eval)(src); return true; }}
                    return false;
                }}
            }};

            // ---- Web APIs ----
            // Pending fetch resolvers and timer callbacks live in these global
            // registries (they survive every `execute_script` call in the shared
            // context), so Rust never stores JS function handles.
            if (!globalThis.__pwb_pending) globalThis.__pwb_pending = {{}};
            if (!globalThis.__pwb_timers) globalThis.__pwb_timers = {{}};

            globalThis.fetch = function(url, options) {{
                options = options || {{}};
                return new Promise(function(resolve, reject) {{
                    var id = __pwb_nextOpId();
                    globalThis.__pwb_pending[id] = {{ resolve: resolve, reject: reject }};
                    var method = String(options.method || "GET").toUpperCase();
                    var headers = options.headers || {{}};
                    var headerObj = {{}};
                    for (var k in headers) {{
                        if (Object.prototype.hasOwnProperty.call(headers, k)) headerObj[k] = String(headers[k]);
                    }}
                    var body = options.body != null ? String(options.body) : "";
                    __pwb_fetchStart(id, String(url), method, JSON.stringify(headerObj), body);
                }}).then(function(raw) {{
                    return {{
                        ok: raw.ok,
                        status: raw.status,
                        statusText: raw.statusText,
                        url: raw.url,
                        headers: raw._headers || {{}},
                        text: function() {{ return Promise.resolve(raw._body); }},
                        json: function() {{ return Promise.resolve(JSON.parse(raw._body)); }},
                        arrayBuffer: function() {{
                            var bytes = new Uint8Array(raw._body.length);
                            for (var i = 0; i < raw._body.length; i++) bytes[i] = raw._body.charCodeAt(i);
                            return Promise.resolve(bytes.buffer);
                        }}
                    }};
                }});
            }};

            globalThis.setTimeout = function(fn, ms) {{
                var id = __pwb_nextOpId();
                globalThis.__pwb_timers[id] = fn;
                __pwb_setTimer(id, ms || 0, false);
                return id;
            }};
            globalThis.setInterval = function(fn, ms) {{
                var id = __pwb_nextOpId();
                globalThis.__pwb_timers[id] = fn;
                __pwb_setTimer(id, ms || 0, true);
                return id;
            }};
            globalThis.clearTimeout = function(id) {{ delete globalThis.__pwb_timers[id]; __pwb_clearTimer(id); }};
            globalThis.clearInterval = function(id) {{ delete globalThis.__pwb_timers[id]; __pwb_clearTimer(id); }};

            function makeStorage(kind) {{
                return {{
                    getItem: function(k) {{ return __pwb_storage(kind, "get", String(k), ""); }},
                    setItem: function(k, v) {{ __pwb_storage(kind, "set", String(k), String(v)); }},
                    removeItem: function(k) {{ __pwb_storage(kind, "remove", String(k), ""); }},
                    clear: function() {{ __pwb_storage(kind, "clear", "", ""); }},
                    key: function(i) {{ return __pwb_storage(kind, "key", String(i), ""); }},
                    get length() {{ return parseInt(__pwb_storage(kind, "length", "", ""), 10) || 0; }}
                }};
            }}
            globalThis.localStorage = makeStorage("local");
            globalThis.sessionStorage = makeStorage("session");

            globalThis.alert = function(msg) {{ __pwb_alert(msg == null ? "" : String(msg)); }};
            globalThis.confirm = function(msg) {{ return __pwb_confirm(msg == null ? "" : String(msg)); }};
            globalThis.prompt = function(msg, def) {{ return __pwb_prompt(msg == null ? "" : String(msg), def == null ? "" : String(def)); }};

            globalThis.crypto = {{
                getRandomValues: function(arr) {{
                    if (!arr || typeof arr.length !== "number" || arr.length === 0) return arr;
                    var s = __pwb_randomBytes(arr.length);
                    for (var i = 0; i < arr.length; i++) arr[i] = s.charCodeAt(i);
                    return arr;
                }},
                randomUUID: function() {{ return __pwb_randomUUID(); }}
            }};
            globalThis.btoa = function(s) {{
                var r = __pwb_btoa(String(s));
                if (!r[0]) throw new Error(r[2]);
                return r[1];
            }};
            globalThis.atob = function(s) {{
                var r = __pwb_atob(String(s));
                if (!r[0]) throw new Error(r[2]);
                return r[1];
            }};
            globalThis.performance = {{ now: function() {{ return __pwb_performanceNow(); }} }};

            // ---- WebSocket ----
            if (!globalThis.__pwb_wsObjects) globalThis.__pwb_wsObjects = {{}};
            globalThis.WebSocket = function(url) {{
                this.url = String(url);
                this.readyState = 0;
                this.bufferedAmount = 0;
                this._id = __pwb_wsOpen(this.url);
                globalThis.__pwb_wsObjects[this._id] = this;
            }};
            WebSocket.CONNECTING = 0;
            WebSocket.OPEN = 1;
            WebSocket.CLOSING = 2;
            WebSocket.CLOSED = 3;
            WebSocket.prototype.send = function(data) {{ __pwb_wsSend(this._id, String(data)); this.bufferedAmount = 0; }};
            WebSocket.prototype.close = function() {{
                if (this.readyState !== 3) {{ this.readyState = 2; __pwb_wsClose(this._id); }}
            }};
            globalThis.__pwb_wsOnEvent = function(id, type, data) {{
                var ws = globalThis.__pwb_wsObjects[id];
                if (!ws) return;
                if (type === "open") {{
                    ws.readyState = 1;
                    if (ws.onopen) ws.onopen({{ type: "open" }});
                }} else if (type === "message") {{
                    if (ws.onmessage) ws.onmessage({{ data: data, type: "message" }});
                }} else if (type === "error") {{
                    if (ws.onerror) ws.onerror({{ type: "error", message: data }});
                }} else if (type === "close") {{
                    ws.readyState = 3;
                    if (ws.onclose) ws.onclose({{ type: "close" }});
                    delete globalThis.__pwb_wsObjects[id];
                }}
            }};
            "##,
        title = js_escape(title),
    )
}
