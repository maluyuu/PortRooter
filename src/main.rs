use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{self, header, HeaderName, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use http_body_util::BodyExt;
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::TokioExecutor,
};
use serde::Deserialize;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::time::timeout;

#[derive(Debug, Deserialize, Clone)]
struct Config {
    router_port: u16,
    targets: Vec<Target>,
}

#[derive(Debug, Deserialize, Clone)]
struct Target {
    name: String,
    port: u16,
    description: String,
}

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    client: Client<HttpConnector, Body>,
}

#[tokio::main]
async fn main() {
    // 設定ファイルを読み込み
    let config_str = std::fs::read_to_string("config.toml")
        .expect("config.tomlを読み込めませんでした");
    let config: Config = toml::from_str(&config_str)
        .expect("config.tomlのパースに失敗しました");

    println!("🚀 PortRooter を起動中...");
    println!("📝 集約ポート: {}", config.router_port);
    println!("📋 登録されたターゲット:");
    for target in &config.targets {
        println!("  - {} (localhost:{}): {}", target.name, target.port, target.description);
    }

    let client = Client::builder(TokioExecutor::new()).build_http();

    let state = AppState {
        config: Arc::new(config.clone()),
        client,
    };

    // ルーター設定
    let app = Router::new()
        .route("/", get(show_selector))
        .route("/proxy/:target_name", get(proxy_handler).post(proxy_handler))
        .route("/proxy/:target_name/*path", get(proxy_handler).post(proxy_handler).put(proxy_handler).delete(proxy_handler).patch(proxy_handler))
        .fallback(get(fallback_handler).post(fallback_handler).put(fallback_handler).delete(fallback_handler).patch(fallback_handler))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], config.router_port));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    println!("\n✅ サーバー起動完了!");
    println!("🌐 http://localhost:{} にアクセスしてください\n", config.router_port);

    axum::serve(listener, app).await.unwrap();
}

// ターゲット選択UIを表示
async fn show_selector(State(state): State<AppState>) -> Html<String> {
    let mut html = String::from(r#"
<!DOCTYPE html>
<html lang="ja">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>PortRooter - ポート選択</title>
    <style>
        * {
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            padding: 20px;
        }
        .container {
            background: white;
            border-radius: 16px;
            box-shadow: 0 20px 60px rgba(0, 0, 0, 0.3);
            max-width: 800px;
            width: 100%;
            padding: 40px;
        }
        h1 {
            color: #333;
            margin-bottom: 10px;
            font-size: 32px;
        }
        .subtitle {
            color: #666;
            margin-bottom: 30px;
            font-size: 16px;
        }
        .targets {
            display: grid;
            gap: 16px;
        }
        .target-card {
            background: #f8f9fa;
            border: 2px solid transparent;
            border-radius: 12px;
            padding: 20px;
            cursor: pointer;
            transition: all 0.3s ease;
            text-decoration: none;
            color: inherit;
            display: block;
        }
        .target-card:hover {
            border-color: #667eea;
            transform: translateY(-2px);
            box-shadow: 0 4px 12px rgba(102, 126, 234, 0.2);
        }
        .target-name {
            font-size: 20px;
            font-weight: 600;
            color: #333;
            margin-bottom: 8px;
        }
        .target-port {
            font-size: 14px;
            color: #667eea;
            font-weight: 500;
            margin-bottom: 8px;
        }
        .target-description {
            font-size: 14px;
            color: #666;
        }
        .icon {
            margin-right: 8px;
        }
    </style>
</head>
<body>
    <div class="container">
        <h1>🚀 PortRooter</h1>
        <p class="subtitle">アクセスしたいポートを選択してください</p>
        <div class="targets">
"#);

    for target in &state.config.targets {
        html.push_str(&format!(
            r#"
            <a href="/proxy/{}" class="target-card">
                <div class="target-name"><span class="icon">🎯</span>{}</div>
                <div class="target-port">localhost:{}</div>
                <div class="target-description">{}</div>
            </a>
"#,
            urlencoding::encode(&target.name),
            html_escape::encode_text(&target.name),
            target.port,
            html_escape::encode_text(&target.description)
        ));
    }

    html.push_str(
        r#"
        </div>
    </div>
</body>
</html>
"#,
    );

    Html(html)
}

// フォールバックハンドラー（リファラーベースのルーティング）
async fn fallback_handler(
    State(state): State<AppState>,
    mut req: Request,
) -> Result<Response, StatusCode> {
    // リファラーヘッダーから対象ターゲットを抽出
    let referer = req.headers()
        .get(header::REFERER)
        .and_then(|r| r.to_str().ok())
        .unwrap_or("");

    // リファラーから /proxy/{target_name} の部分を抽出
    let target_name = if let Some(proxy_pos) = referer.find("/proxy/") {
        let start = proxy_pos + "/proxy/".len();
        let remaining = &referer[start..];
        // 次の / または末尾までを取得
        if let Some(slash_pos) = remaining.find('/') {
            urlencoding::decode(&remaining[..slash_pos])
                .ok()
                .map(|s| s.to_string())
        } else {
            urlencoding::decode(remaining)
                .ok()
                .map(|s| s.to_string())
        }
    } else {
        // Refererに /proxy/ が含まれていない場合、
        // Originヘッダーをチェックしてプロキシ経由かどうか判断
        let origin = req.headers()
            .get(header::ORIGIN)
            .and_then(|o| o.to_str().ok())
            .unwrap_or("");

        // Originがプロキシサーバーのポートの場合、デフォルトターゲット（最初のターゲット）を使用
        if origin.contains(&format!(":{}", state.config.router_port)) ||
           referer.contains(&format!(":{}", state.config.router_port)) {
            state.config.targets.first().map(|t| t.name.clone())
        } else {
            None
        }
    };

    if let Some(target_name) = target_name {
        // ターゲットを検索
        if let Some(target) = state.config.targets.iter().find(|t| t.name == target_name) {
            // リクエストパスを取得（そのまま使う）
            let request_path = req.uri().path().to_string();
            let query = req.uri().query()
                .map(|q| format!("?{}", q))
                .unwrap_or_default();

            // プロキシURIを構築
            let proxy_uri = format!("http://localhost:{}{}{}", target.port, request_path, query);

            println!("🔄 フォールバック: {} -> {}", req.uri(), proxy_uri);

            let original_host = req.headers()
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("localhost")
                .to_string();

            // URIを更新
            *req.uri_mut() = proxy_uri.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

            // ヘッダーを適切に設定
            let headers = req.headers_mut();

            // Accept-Encodingヘッダーを削除（圧縮を無効化）
            headers.remove(header::ACCEPT_ENCODING);

            // ホストヘッダーを更新
            headers.insert(
                header::HOST,
                format!("localhost:{}", target.port)
                    .parse()
                    .map_err(|_| StatusCode::BAD_REQUEST)?,
            );

            // X-Forwarded-* ヘッダーを追加
            headers.insert(
                HeaderName::from_static("x-forwarded-for"),
                "127.0.0.1".parse().unwrap(),
            );
            headers.insert(
                HeaderName::from_static("x-forwarded-proto"),
                "http".parse().unwrap(),
            );
            headers.insert(
                HeaderName::from_static("x-forwarded-host"),
                original_host.as_str().parse().unwrap(),
            );

            // Originヘッダーを更新
            if headers.contains_key(header::ORIGIN) {
                headers.insert(
                    header::ORIGIN,
                    format!("http://localhost:{}", target.port)
                        .parse()
                        .map_err(|_| StatusCode::BAD_REQUEST)?,
                );
            }

            // Refererヘッダーを更新
            if let Some(ref referer_value) = headers.get(header::REFERER).and_then(|r| r.to_str().ok()) {
                if let Ok(referer_uri) = referer_value.parse::<http::Uri>() {
                    let new_referer = format!(
                        "http://localhost:{}{}",
                        target.port,
                        referer_uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("")
                    );
                    headers.insert(
                        header::REFERER,
                        new_referer.parse().map_err(|_| StatusCode::BAD_REQUEST)?,
                    );
                }
            }

            // プロキシリクエストを送信（10秒のタイムアウト）
            let response = match timeout(Duration::from_secs(10), state.client.request(req)).await {
                Ok(Ok(response)) => {
                    println!("✅ フォールバック成功: ステータス {}", response.status());
                    response
                }
                Ok(Err(err)) => {
                    eprintln!("❌ フォールバックプロキシエラー: {} -> {}", proxy_uri, err);
                    eprintln!("   詳細: {:?}", err);
                    let error_body = format!("プロキシエラー: バックエンドサーバー {}:{} に接続できません\n詳細: {}",
                        target.name, target.port, err);
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_GATEWAY)
                        .body(Body::from(error_body))
                        .unwrap()
                        .into_response());
                }
                Err(_) => {
                    eprintln!("❌ フォールバックタイムアウト: {} (10秒)", proxy_uri);
                    let error_body = format!("タイムアウト: バックエンドサーバー {}:{} が応答しません（10秒）",
                        target.name, target.port);
                    return Ok(Response::builder()
                        .status(StatusCode::GATEWAY_TIMEOUT)
                        .body(Body::from(error_body))
                        .unwrap()
                        .into_response());
                }
            };

            // レスポンスを取得
            let (mut parts, body) = response.into_parts();

            // CSPヘッダーを削除
            parts.headers.remove(header::CONTENT_SECURITY_POLICY);
            parts.headers.remove(HeaderName::from_static("content-security-policy-report-only"));

            // Cross-Origin-Resource-Policyヘッダーを追加（SharedArrayBuffer/WASM対応）
            if !parts.headers.contains_key(HeaderName::from_static("cross-origin-resource-policy")) {
                parts.headers.insert(
                    HeaderName::from_static("cross-origin-resource-policy"),
                    "cross-origin".parse().unwrap(),
                );
            }

            let content_type = parts.headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");

            let proxy_prefix = format!("/proxy/{}", urlencoding::encode(&target.name));

            // JavaScript/TypeScriptファイルの場合、import文を変換
            if content_type.contains("javascript") || content_type.contains("typescript")
               || request_path.ends_with(".js") || request_path.ends_with(".mjs")
               || request_path.ends_with(".ts") || request_path.ends_with(".tsx")
               || request_path.contains(".js?") || request_path.contains(".mjs?")
               || request_path.contains(".ts?") || request_path.contains(".tsx?") {
                let body_bytes = match body.collect().await {
                    Ok(collected) => collected.to_bytes(),
                    Err(err) => {
                        eprintln!("❌ フォールバックJavaScriptボディ読み取りエラー: {:?}", err);
                        let error_body = "JavaScriptレスポンスの読み取りに失敗しました";
                        return Ok(Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(error_body))
                            .unwrap()
                            .into_response());
                    }
                };

                let mut content = String::from_utf8_lossy(&body_bytes).to_string();

                // Viteのプリバンドルファイル（node_modules/.vite/deps/）は変換しない
                let is_vite_deps = request_path.contains("/node_modules/.vite/deps/");

                if !is_vite_deps {
                    // import/export文の絶対パスを変換
                    // from '/...' を from '/proxy/{target}/...' に変換
                    content = content.replace("from '/", &format!("from '{}/", proxy_prefix));
                    content = content.replace("from \"/", &format!("from \"{}/", proxy_prefix));

                    // import('/...') を import('/proxy/{target}/...') に変換
                    content = content.replace("import('/", &format!("import('{}/", proxy_prefix));
                    content = content.replace("import(\"/", &format!("import(\"{}/", proxy_prefix));

                    // 二重変換を修正
                    content = content.replace(&format!("from '{}/proxy/", proxy_prefix), "from '/proxy/");
                    content = content.replace(&format!("from \"{}/proxy/", proxy_prefix), "from \"/proxy/");
                    content = content.replace(&format!("import('{}/proxy/", proxy_prefix), "import('/proxy/");
                    content = content.replace(&format!("import(\"{}/proxy/", proxy_prefix), "import(\"/proxy/");
                }

                let mut response = Response::new(Body::from(content));
                *response.status_mut() = parts.status;
                *response.headers_mut() = parts.headers;
                response.headers_mut().remove(header::CONTENT_LENGTH);

                return Ok(response);
            } else if content_type.contains("css") || request_path.ends_with(".css") {
                let body_bytes = match body.collect().await {
                    Ok(collected) => collected.to_bytes(),
                    Err(err) => {
                        eprintln!("❌ フォールバックCSSボディ読み取りエラー: {:?}", err);
                        let error_body = "CSSレスポンスの読み取りに失敗しました";
                        return Ok(Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(error_body))
                            .unwrap()
                            .into_response());
                    }
                };

                let mut content = String::from_utf8_lossy(&body_bytes).to_string();

                // url()と@importを変換
                content = content.replace("url('/", &format!("url('{}/", proxy_prefix));
                content = content.replace("url(\"/", &format!("url(\"{}/", proxy_prefix));
                content = content.replace("url(/", &format!("url({}/", proxy_prefix));
                content = content.replace("@import '/", &format!("@import '{}/", proxy_prefix));
                content = content.replace("@import \"/", &format!("@import \"{}/", proxy_prefix));

                // 二重変換を修正
                content = content.replace(&format!("url('{}/proxy/", proxy_prefix), "url('/proxy/");
                content = content.replace(&format!("url(\"{}/proxy/", proxy_prefix), "url(\"/proxy/");
                content = content.replace(&format!("url({}/proxy/", proxy_prefix), "url(/proxy/");
                content = content.replace(&format!("@import '{}/proxy/", proxy_prefix), "@import '/proxy/");
                content = content.replace(&format!("@import \"{}/proxy/", proxy_prefix), "@import \"/proxy/");

                let mut response = Response::new(Body::from(content));
                *response.status_mut() = parts.status;
                *response.headers_mut() = parts.headers;
                response.headers_mut().remove(header::CONTENT_LENGTH);

                return Ok(response);
            } else {
                // その他のレスポンスはそのまま返す
                let response = Response::from_parts(parts, body);
                return Ok(response.into_response());
            }
        }
    }

    // リファラーがない、または対象ターゲットが見つからない場合は404
    Err(StatusCode::NOT_FOUND)
}

// プロキシハンドラー
async fn proxy_handler(
    State(state): State<AppState>,
    Path(params): Path<std::collections::HashMap<String, String>>,
    mut req: Request,
) -> Result<Response, StatusCode> {
    let target_name = params.get("target_name")
        .ok_or(StatusCode::BAD_REQUEST)?;

    // ターゲットを検索
    let target = state.config.targets
        .iter()
        .find(|t| &t.name == target_name)
        .ok_or(StatusCode::NOT_FOUND)?;

    // リクエストURIから実際のパスを取得
    let request_path = req.uri().path().to_string();
    let encoded_target_name = urlencoding::encode(target_name);
    let prefix = format!("/proxy/{}", encoded_target_name);

    // プレフィックスを除去して、残りのパスを取得
    let path = if request_path.starts_with(&prefix) {
        let remaining = &request_path[prefix.len()..];
        if remaining.is_empty() { "/" } else { remaining }
    } else {
        "/"
    };

    // クエリパラメータを取得
    let query = req.uri().query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();

    // 新しいURIを構築
    let proxy_uri = format!("http://localhost:{}{}{}", target.port, path, query);

    println!("🔄 プロキシ: {} -> {}", req.uri(), proxy_uri);

    let original_host = req.headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost")
        .to_string();

    // URIを更新
    *req.uri_mut() = proxy_uri.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    // ヘッダーを適切に設定
    let headers = req.headers_mut();

    // Accept-Encodingヘッダーを削除（圧縮を無効化）
    // これにより、バックエンドから圧縮されていないレスポンスを受け取る
    headers.remove(header::ACCEPT_ENCODING);

    // ホストヘッダーを更新
    headers.insert(
        header::HOST,
        format!("localhost:{}", target.port)
            .parse()
            .map_err(|_| StatusCode::BAD_REQUEST)?,
    );

    // X-Forwarded-* ヘッダーを追加（プロキシ経由であることを通知）
    headers.insert(
        HeaderName::from_static("x-forwarded-for"),
        "127.0.0.1".parse().unwrap(),
    );
    headers.insert(
        HeaderName::from_static("x-forwarded-proto"),
        "http".parse().unwrap(),
    );
    headers.insert(
        HeaderName::from_static("x-forwarded-host"),
        original_host.as_str().parse().unwrap(),
    );

    // Originヘッダーを更新（存在する場合）
    if headers.contains_key(header::ORIGIN) {
        headers.insert(
            header::ORIGIN,
            format!("http://localhost:{}", target.port)
                .parse()
                .map_err(|_| StatusCode::BAD_REQUEST)?,
        );
    }

    // Refererヘッダーを更新（存在する場合）
    if let Some(referer) = headers.get(header::REFERER).and_then(|r| r.to_str().ok()) {
        // リファラーのパスを保持しつつ、ホスト部分を変更
        if let Ok(referer_uri) = referer.parse::<http::Uri>() {
            let new_referer = format!(
                "http://localhost:{}{}",
                target.port,
                referer_uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("")
            );
            headers.insert(
                header::REFERER,
                new_referer.parse().map_err(|_| StatusCode::BAD_REQUEST)?,
            );
        }
    }

    // プロキシリクエストを送信（10秒のタイムアウト）
    let response = match timeout(Duration::from_secs(10), state.client.request(req)).await {
        Ok(Ok(response)) => {
            println!("✅ プロキシ成功: ステータス {}", response.status());
            response
        }
        Ok(Err(err)) => {
            eprintln!("❌ プロキシエラー: {} -> {}", proxy_uri, err);
            eprintln!("   詳細: {:?}", err);
            let error_body = format!("プロキシエラー: バックエンドサーバー {}:{} に接続できません\n詳細: {}",
                target.name, target.port, err);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from(error_body))
                .unwrap()
                .into_response());
        }
        Err(_) => {
            eprintln!("❌ タイムアウト: {} (10秒)", proxy_uri);
            let error_body = format!("タイムアウト: バックエンドサーバー {}:{} が応答しません（10秒）",
                target.name, target.port);
            return Ok(Response::builder()
                .status(StatusCode::GATEWAY_TIMEOUT)
                .body(Body::from(error_body))
                .unwrap()
                .into_response());
        }
    };

    // レスポンスを取得
    let (mut parts, body) = response.into_parts();

    println!("📦 レスポンス情報:");
    println!("   ステータス: {}", parts.status);
    let content_type = parts.headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();  // Stringに変換して借用を解放
    println!("   Content-Type: {}", if content_type.is_empty() { "(なし)" } else { &content_type });
    println!("   パス: {}", request_path);

    // CSPヘッダーを削除（プロキシ経由でのスクリプト実行を許可）
    parts.headers.remove(header::CONTENT_SECURITY_POLICY);
    parts.headers.remove(HeaderName::from_static("content-security-policy-report-only"));

    // Cross-Origin-Resource-Policyヘッダーを追加（SharedArrayBuffer/WASM対応）
    // Cross-Origin-Embedder-Policy: require-corp と互換性を持たせる
    if !parts.headers.contains_key(HeaderName::from_static("cross-origin-resource-policy")) {
        parts.headers.insert(
            HeaderName::from_static("cross-origin-resource-policy"),
            "cross-origin".parse().unwrap(),
        );
    }

    let proxy_prefix = format!("/proxy/{}", urlencoding::encode(&target.name));

    // HTMLレスポンスの場合、<base>タグを挿入して絶対パスを変換
    if content_type.contains("text/html") {
        println!("🔧 HTML処理を開始");

        // ボディを読み取る
        let body_bytes = match body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(err) => {
                eprintln!("❌ HTMLボディ読み取りエラー: {:?}", err);
                let error_body = "HTMLレスポンスの読み取りに失敗しました";
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from(error_body))
                    .unwrap()
                    .into_response());
            }
        };

        println!("   元のHTMLサイズ: {} bytes", body_bytes.len());


        let html = String::from_utf8_lossy(&body_bytes);

        // <base>タグを挿入
        let base_url = format!("/proxy/{}/", urlencoding::encode(&target.name));
        let base_tag = format!("<base href=\"{}\">", base_url);

        // <head>タグの直後に<base>タグを挿入
        let mut modified_html = if let Some(pos) = html.find("<head>") {
            let insert_pos = pos + "<head>".len();
            format!("{}{}{}", &html[..insert_pos], base_tag, &html[insert_pos..])
        } else if let Some(pos) = html.find("<HEAD>") {
            let insert_pos = pos + "<HEAD>".len();
            format!("{}{}{}", &html[..insert_pos], base_tag, &html[insert_pos..])
        } else {
            // <head>タグが見つからない場合は、<html>タグの直後に挿入
            if let Some(pos) = html.find("<html") {
                if let Some(end_pos) = html[pos..].find('>') {
                    let insert_pos = pos + end_pos + 1;
                    format!("{}<head>{}</head>{}", &html[..insert_pos], base_tag, &html[insert_pos..])
                } else {
                    html.to_string()
                }
            } else {
                html.to_string()
            }
        };

        // CSP metaタグを削除
        // <meta http-equiv="Content-Security-Policy" ...> を削除
        if let Some(start) = modified_html.find("<meta http-equiv=\"Content-Security-Policy\"") {
            if let Some(end) = modified_html[start..].find('>') {
                let end_pos = start + end + 1;
                modified_html = format!("{}{}", &modified_html[..start], &modified_html[end_pos..]);
            }
        }
        // 小文字版も対応
        if let Some(start) = modified_html.find("<meta http-equiv='Content-Security-Policy'") {
            if let Some(end) = modified_html[start..].find('>') {
                let end_pos = start + end + 1;
                modified_html = format!("{}{}", &modified_html[..start], &modified_html[end_pos..]);
            }
        }

        // HTML内の絶対パス（/で始まるパス）をプロキシパスに変換
        // <base>タグは絶対パスに適用されないため、手動で変換する必要がある
        let proxy_prefix = format!("/proxy/{}", urlencoding::encode(&target.name));

        // src="/..." と href="/..." を src="/proxy/{target}/..." と href="/proxy/{target}/..." に変換
        // ただし、すでに /proxy/ で始まっているパスや http:// https:// で始まるURLは変換しない
        modified_html = modified_html.replace("src=\"/", &format!("src=\"{}/", proxy_prefix));
        modified_html = modified_html.replace("href=\"/", &format!("href=\"{}/", proxy_prefix));
        modified_html = modified_html.replace("src='/", &format!("src='{}/", proxy_prefix));
        modified_html = modified_html.replace("href='/", &format!("href='{}/", proxy_prefix));

        // すでにプロキシパスになっている二重変換を修正
        modified_html = modified_html.replace(&format!("src=\"{}/proxy/", proxy_prefix), "src=\"/proxy/");
        modified_html = modified_html.replace(&format!("href=\"{}/proxy/", proxy_prefix), "href=\"/proxy/");
        modified_html = modified_html.replace(&format!("src='{}/proxy/", proxy_prefix), "src='/proxy/");
        modified_html = modified_html.replace(&format!("href='{}/proxy/", proxy_prefix), "href='/proxy/");

        // JavaScriptコード内の絶対パスもプロキシパスに変換
        // fetch('/api/') を fetch('/proxy/{target}/api/') に変換
        modified_html = modified_html.replace("fetch('/", &format!("fetch('{}/", proxy_prefix));
        modified_html = modified_html.replace("fetch(\"/", &format!("fetch(\"{}/", proxy_prefix));
        // すでにプロキシパスになっている二重変換を修正
        modified_html = modified_html.replace(&format!("fetch('{}/proxy/", proxy_prefix), "fetch('/proxy/");
        modified_html = modified_html.replace(&format!("fetch(\"{}/proxy/", proxy_prefix), "fetch(\"/proxy/");

        // XMLHttpRequestの場合も対応
        modified_html = modified_html.replace(".open('GET', '/", &format!(".open('GET', '{}/", proxy_prefix));
        modified_html = modified_html.replace(".open('POST', '/", &format!(".open('POST', '{}/", proxy_prefix));
        modified_html = modified_html.replace(".open(\"GET\", \"/", &format!(".open(\"GET\", \"{}/", proxy_prefix));
        modified_html = modified_html.replace(".open(\"POST\", \"/", &format!(".open(\"POST\", \"{}/", proxy_prefix));
        // 二重変換を修正
        modified_html = modified_html.replace(&format!(".open('GET', '{}/proxy/", proxy_prefix), ".open('GET', '/proxy/");
        modified_html = modified_html.replace(&format!(".open('POST', '{}/proxy/", proxy_prefix), ".open('POST', '/proxy/");
        modified_html = modified_html.replace(&format!(".open(\"GET\", \"{}/proxy/", proxy_prefix), ".open(\"GET\", \"/proxy/");
        modified_html = modified_html.replace(&format!(".open(\"POST\", \"{}/proxy/", proxy_prefix), ".open(\"POST\", \"/proxy/");

        // 新しいレスポンスを作成
        println!("   変換後のHTMLサイズ: {} bytes", modified_html.len());
        let mut response = Response::new(Body::from(modified_html));
        *response.status_mut() = parts.status;
        *response.headers_mut() = parts.headers;

        // Content-Lengthを更新（変更されている可能性があるため）
        response.headers_mut().remove(header::CONTENT_LENGTH);

        println!("✅ HTMLレスポンスを返却");
        Ok(response)
    } else if content_type.contains("css") || request_path.ends_with(".css") {
        println!("🔧 CSS処理を開始");
        // CSSファイルの場合、url()と@importの絶対パスを変換
        let body_bytes = match body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(err) => {
                eprintln!("❌ CSSボディ読み取りエラー: {:?}", err);
                let error_body = "CSSレスポンスの読み取りに失敗しました";
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from(error_body))
                    .unwrap()
                    .into_response());
            }
        };

        let mut content = String::from_utf8_lossy(&body_bytes).to_string();

        // url('/path') を url('/proxy/{target}/path') に変換
        content = content.replace("url('/", &format!("url('{}/", proxy_prefix));
        content = content.replace("url(\"/", &format!("url(\"{}/", proxy_prefix));
        content = content.replace("url(/", &format!("url({}/", proxy_prefix));

        // @import '/path' を @import '/proxy/{target}/path' に変換
        content = content.replace("@import '/", &format!("@import '{}/", proxy_prefix));
        content = content.replace("@import \"/", &format!("@import \"{}/", proxy_prefix));

        // 二重変換を修正
        content = content.replace(&format!("url('{}/proxy/", proxy_prefix), "url('/proxy/");
        content = content.replace(&format!("url(\"{}/proxy/", proxy_prefix), "url(\"/proxy/");
        content = content.replace(&format!("url({}/proxy/", proxy_prefix), "url(/proxy/");
        content = content.replace(&format!("@import '{}/proxy/", proxy_prefix), "@import '/proxy/");
        content = content.replace(&format!("@import \"{}/proxy/", proxy_prefix), "@import \"/proxy/");

        let mut response = Response::new(Body::from(content));
        *response.status_mut() = parts.status;
        *response.headers_mut() = parts.headers;
        response.headers_mut().remove(header::CONTENT_LENGTH);

        println!("✅ CSSレスポンスを返却");
        Ok(response)
    } else if content_type.contains("javascript") || content_type.contains("typescript")
           || request_path.ends_with(".js") || request_path.ends_with(".mjs")
           || request_path.ends_with(".ts") || request_path.ends_with(".tsx")
           || request_path.contains(".js?") || request_path.contains(".mjs?")
           || request_path.contains(".ts?") || request_path.contains(".tsx?") {
        println!("🔧 JavaScript処理を開始");
        // JavaScript/TypeScript ファイルの場合、import文の絶対パスを変換
        let body_bytes = match body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(err) => {
                eprintln!("❌ JavaScriptボディ読み取りエラー: {:?}", err);
                let error_body = "JavaScriptレスポンスの読み取りに失敗しました";
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from(error_body))
                    .unwrap()
                    .into_response());
            }
        };

        let mut content = String::from_utf8_lossy(&body_bytes).to_string();

        // Viteのプリバンドルファイル（node_modules/.vite/deps/）は変換しない
        // これらのファイルは既に処理されており、変換すると壊れる可能性がある
        let is_vite_deps = request_path.contains("/node_modules/.vite/deps/");

        if !is_vite_deps {
            // import/export文の絶対パスをプロキシパスに変換
            // from '/...' を from '/proxy/{target}/...' に変換（import/export両方に適用）
            content = content.replace("from '/", &format!("from '{}/", proxy_prefix));
            content = content.replace("from \"/", &format!("from \"{}/", proxy_prefix));

            // import('/...') を import('/proxy/{target}/...') に変換
            content = content.replace("import('/", &format!("import('{}/", proxy_prefix));
            content = content.replace("import(\"/", &format!("import(\"{}/", proxy_prefix));

            // 二重変換を修正
            content = content.replace(&format!("from '{}/proxy/", proxy_prefix), "from '/proxy/");
            content = content.replace(&format!("from \"{}/proxy/", proxy_prefix), "from \"/proxy/");
            content = content.replace(&format!("import('{}/proxy/", proxy_prefix), "import('/proxy/");
            content = content.replace(&format!("import(\"{}/proxy/", proxy_prefix), "import(\"/proxy/");
        }

        // 新しいレスポンスを作成
        let mut response = Response::new(Body::from(content));
        *response.status_mut() = parts.status;
        *response.headers_mut() = parts.headers;
        response.headers_mut().remove(header::CONTENT_LENGTH);

        println!("✅ JavaScriptレスポンスを返却");
        Ok(response)
    } else {
        println!("🔧 その他のファイル（変換なし）");
        // その他のレスポンスはそのまま返す
        let response = Response::from_parts(parts, body);
        println!("✅ レスポンスを返却");
        Ok(response.into_response())
    }
}
