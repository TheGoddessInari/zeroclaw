use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use std::time::Duration;

/// Web search tool for searching the internet.
/// Supports multiple providers: DuckDuckGo (free), Brave (requires API key).
pub struct WebSearchTool {
    provider: String,
    brave_api_key: Option<String>,
    max_results: usize,
    timeout_secs: u64,
}

impl WebSearchTool {
    pub fn new(
        provider: String,
        brave_api_key: Option<String>,
        max_results: usize,
        timeout_secs: u64,
    ) -> Self {
        Self {
            provider: provider.trim().to_lowercase(),
            brave_api_key,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
        }
    }

    async fn search_duckduckgo(&self, query: &str) -> anyhow::Result<String> {
        let encoded_query = urlencoding::encode(query);
        let search_url = format!("https://html.duckduckgo.com/html/?q={}", encoded_query);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()?;

        let response = client.get(&search_url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "DuckDuckGo search failed with status: {}",
                response.status()
            );
        }

        let html = response.text().await?;
        self.parse_duckduckgo_results(&html, query)
    }

    fn parse_duckduckgo_results(&self, html: &str, query: &str) -> anyhow::Result<String> {
        // Extract result links: <a class="result__a" href="...">Title</a>
        let link_regex = Regex::new(
            r#"<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#,
        )?;

        // Extract snippets: <a class="result__snippet">...</a>
        let snippet_regex = Regex::new(r#"<a class="result__snippet[^"]*"[^>]*>([\s\S]*?)</a>"#)?;

        let link_matches: Vec<_> = link_regex.captures_iter(html).collect();

        if link_matches.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via DuckDuckGo)", query)];
        let count = link_matches.len().min(self.max_results);

        for i in 0..count {
            let link_cap = &link_matches[i];
            let link_match = link_cap.get(0).unwrap();
            let link_end = link_match.end();

            // Determine the end of the search range for the snippet
            // If there is a next link, stop before it. Otherwise, search until the end of the HTML.
            let next_start = if i + 1 < link_matches.len() {
                link_matches[i + 1].get(0).unwrap().start()
            } else {
                html.len()
            };

            let snippet_search_area = &html[link_end..next_start];

            let url_str = decode_ddg_redirect_url(&link_cap[1]);
            let title = strip_tags(&link_cap[2]);

            lines.push(format!("{}. {}", i + 1, title.trim()));
            lines.push(format!("   {}", url_str.trim()));

            // Find snippet in the specific area between this link and the next
            if let Some(snippet_cap) = snippet_regex.captures(snippet_search_area) {
                let snippet = strip_tags(&snippet_cap[1]);
                let snippet = snippet.trim();
                if !snippet.is_empty() {
                    lines.push(format!("   {}", snippet));
                }
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search_brave(&self, query: &str) -> anyhow::Result<String> {
        let api_key = self
            .brave_api_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Brave API key not configured"))?;

        let encoded_query = urlencoding::encode(query);
        let search_url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            encoded_query, self.max_results
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .build()?;

        let response = client
            .get(&search_url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Brave search failed with status: {}", response.status());
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_brave_results(&json, query)
    }

    fn parse_brave_results(&self, json: &serde_json::Value, query: &str) -> anyhow::Result<String> {
        let results = json
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid Brave API response"))?;

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Brave)", query)];

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let description = result
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("");

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !description.is_empty() {
                lines.push(format!("   {}", description));
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search_duckduckgo_news(&self, query: &str) -> anyhow::Result<String> {
        let vqd = self.fetch_vqd(query).await?;
        let encoded_query = urlencoding::encode(query);
        let url = format!(
            "https://duckduckgo.com/news.js?l=wt-wt&o=json&q={}&vqd={}&p=-1&s=0",
            encoded_query, vqd
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()?;

        let response = client
            .get(&url)
            .header("Referer", "https://duckduckgo.com/")
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("DuckDuckGo news search failed with status: {}", response.status());
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_duckduckgo_news_results(&json, query)
    }

    fn parse_duckduckgo_news_results(
        &self,
        json: &serde_json::Value,
        query: &str,
    ) -> anyhow::Result<String> {
        let results = json
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid DuckDuckGo news response"))?;

        if results.is_empty() {
            return Ok(format!("No news results found for: {}", query));
        }

        let mut lines = vec![format!("News results for: {} (via DuckDuckGo)", query)];

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let excerpt = result
                .get("excerpt")
                .and_then(|e| e.as_str())
                .map(strip_tags)
                .unwrap_or_default();
            let source = result
                .get("source")
                .and_then(|s| s.as_str())
                .unwrap_or("Unknown Source");
            let relative_time = result
                .get("relative_time")
                .and_then(|t| t.as_str())
                .unwrap_or("");

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   Source: {} ({})", source, relative_time));
            lines.push(format!("   {}", url));
            if !excerpt.is_empty() {
                lines.push(format!("   {}", excerpt));
            }
        }

        Ok(lines.join("\n"))
    }

    async fn fetch_vqd(&self, query: &str) -> anyhow::Result<String> {
        let encoded_query = urlencoding::encode(query);
        let url = format!("https://duckduckgo.com/?q={}", encoded_query);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()?;

        let response = client
            .get(&url)
            .header("Referer", "https://duckduckgo.com/")
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to fetch VQD token: {}", response.status());
        }

        let text = response.text().await?;
        let re = Regex::new(r#"vqd=['"]([a-zA-Z0-9-]+)['"]"#)?;

        re.captures(&text)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| anyhow::anyhow!("No vqd found in DDG search page"))
    }
}

fn decode_ddg_redirect_url(raw_url: &str) -> String {
    if let Some(index) = raw_url.find("uddg=") {
        let encoded = &raw_url[index + 5..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        if let Ok(decoded) = urlencoding::decode(encoded) {
            return decoded.into_owned();
        }
    }

    raw_url.to_string()
}

fn strip_tags(content: &str) -> String {
    let re = Regex::new(r"<[^>]+>").unwrap();
    re.replace_all(content, "").to_string()
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search_tool"
    }

    fn description(&self) -> &str {
        "Search the web for information. Returns relevant search results with titles, URLs, and descriptions. Use this to find current information, news, or research topics."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query. Be specific for better results."
                },
                "search_type": {
                    "type": "string",
                    "enum": ["text", "news"],
                    "description": "The type of search to perform. Defaults to 'text'. Use 'news' for finding current news articles.",
                    "default": "text"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;

        let search_type = args
            .get("search_type")
            .and_then(|t| t.as_str())
            .unwrap_or("text");

        if query.trim().is_empty() {
            anyhow::bail!("Search query cannot be empty");
        }

        tracing::info!("Searching web ({}) for: {}", search_type, query);

        let result = match self.provider.as_str() {
            "duckduckgo" | "ddg" => {
                if search_type.eq_ignore_ascii_case("news") {
                    self.search_duckduckgo_news(query).await?
                } else {
                    self.search_duckduckgo(query).await?
                }
            }
            "brave" => self.search_brave(query).await?, // Brave implementation doesn't support news mode yet
            _ => anyhow::bail!(
                "Unknown search provider: '{}'. Set tools.web_search.provider to 'duckduckgo' or 'brave' in config.toml",
                self.provider
            ),
        };

        Ok(ToolResult {
            success: true,
            output: result,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_name() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        assert_eq!(tool.name(), "web_search_tool");
    }

    #[test]
    fn test_tool_description() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        assert!(tool.description().contains("Search the web"));
    }

    #[test]
    fn test_parameters_schema() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["properties"]["search_type"].is_object());
    }

    #[test]
    fn test_strip_tags() {
        let html = "<b>Hello</b> <i>World</i>";
        assert_eq!(strip_tags(html), "Hello World");
    }

    #[test]
    fn test_parse_duckduckgo_results_empty() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let result = tool
            .parse_duckduckgo_results("<html>No results here</html>", "test")
            .unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_duckduckgo_results_with_data() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("Example Title"));
        assert!(result.contains("https://example.com"));
    }

    #[test]
    fn test_parse_duckduckgo_results_decodes_redirect_url() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let html = r#"
            <a class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpath%3Fa%3D1&amp;rut=test">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("https://example.com/path?a=1"));
        assert!(!result.contains("rut=test"));
    }

    #[test]
    fn test_constructor_clamps_web_search_limits() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 0, 0);
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("Example Title"));
    }

    #[tokio::test]
    async fn test_execute_missing_query() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_empty_query() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let result = tool.execute(json!({"query": ""})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_brave_without_api_key() {
        let tool = WebSearchTool::new("brave".to_string(), None, 5, 15);
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key"));
    }

    #[test]
    fn test_parse_duckduckgo_results_misalignment() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let html = r#"
            <div class="result">
                <a class="result__a" href="https://example.com/1">Result 1</a>
                <!-- No snippet here -->
            </div>
            <div class="result">
                <a class="result__a" href="https://example.com/2">Result 2</a>
                <a class="result__snippet">Snippet 2</a>
            </div>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();

        // With the bug, Result 1 gets Snippet 2.
        // Result:
        // 1. Result 1
        //    https://example.com/1
        //    Snippet 2
        // 2. Result 2
        //    https://example.com/2

        // We want to assert that Result 1 does NOT have Snippet 2.
        // And Result 2 DOES have Snippet 2.

        let lines: Vec<&str> = result.lines().collect();
        let mut result1_lines = Vec::new();
        let mut result2_lines = Vec::new();
        let mut current_result = 0;

        for line in lines {
            if line.starts_with("1. ") {
                current_result = 1;
            } else if line.starts_with("2. ") {
                current_result = 2;
            }

            if current_result == 1 {
                result1_lines.push(line);
            } else if current_result == 2 {
                result2_lines.push(line);
            }
        }

        let res1_str = result1_lines.join("\n");
        let res2_str = result2_lines.join("\n");

        assert!(!res1_str.contains("Snippet 2"), "Result 1 should not have Snippet 2, but got:\n{}", res1_str);
        assert!(res2_str.contains("Snippet 2"), "Result 2 should have Snippet 2, but got:\n{}", res2_str);
    }

    #[test]
    fn test_parse_duckduckgo_news_results_success() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let json = json!({
            "results": [
                {
                    "title": "News Title 1",
                    "url": "https://news.com/1",
                    "excerpt": "This is a news excerpt.",
                    "source": "News Source",
                    "relative_time": "1 hour ago",
                    "date": 1234567890
                },
                {
                    "title": "News Title 2",
                    "url": "https://news.com/2",
                    "source": "Another Source",
                    "relative_time": "2 hours ago"
                    // missing excerpt
                }
            ]
        });

        let result = tool.parse_duckduckgo_news_results(&json, "test").unwrap();
        assert!(result.contains("News results for: test"));
        assert!(result.contains("News Title 1"));
        assert!(result.contains("This is a news excerpt"));
        assert!(result.contains("News Source"));
        assert!(result.contains("1 hour ago"));
        assert!(result.contains("News Title 2"));
        assert!(result.contains("Another Source"));
    }

    #[test]
    fn test_parse_duckduckgo_news_results_empty() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, 5, 15);
        let json = json!({ "results": [] });
        let result = tool.parse_duckduckgo_news_results(&json, "test").unwrap();
        assert!(result.contains("No news results found"));
    }
}
