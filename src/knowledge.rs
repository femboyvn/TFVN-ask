//! Short sourced lookups for English words and encyclopedia topics.

use reqwest::{header::USER_AGENT, Client, StatusCode, Url};
use serde_json::{json, Value};
use std::time::Duration;

const WIKIPEDIA_API: &str = "https://en.wikipedia.org/w/api.php";
const WIKIMEDIA_USER_AGENT: &str = concat!(
    "KnownSphereBot/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/kienfssc/TFVN-ask)"
);

pub async fn define_word(client: &Client, word: &str) -> Result<Value, String> {
    let word = word.trim();
    if word.is_empty()
        || word.chars().count() > 60
        || !word
            .chars()
            .all(|ch| ch.is_alphabetic() || matches!(ch, ' ' | '-' | '\'' | '’'))
    {
        return Err("Provide an English word of 1 through 60 letters".into());
    }
    let url = dictionary_url(word)?;
    let response = client
        .get(url.clone())
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| "Dictionary service is unavailable right now")?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(format!("No English definition found for {word}"));
    }
    if !response.status().is_success() {
        return Err(format!(
            "Dictionary service returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let payload = response
        .json::<Value>()
        .await
        .map_err(|_| "Dictionary service returned invalid data")?;
    parse_dictionary(&payload, url.as_str())
}

fn dictionary_url(word: &str) -> Result<Url, String> {
    let mut url =
        Url::parse("https://api.dictionaryapi.dev/api/v2/entries/en").map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "Invalid dictionary URL")?
        .push(word);
    Ok(url)
}

fn parse_dictionary(payload: &Value, source_url: &str) -> Result<Value, String> {
    let entry = payload
        .as_array()
        .and_then(|entries| entries.first())
        .ok_or("Dictionary service returned no entry")?;
    let word = entry
        .get("word")
        .and_then(Value::as_str)
        .ok_or("Dictionary service returned no word")?;
    let phonetic = entry.get("phonetic").and_then(Value::as_str).or_else(|| {
        entry
            .get("phonetics")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find_map(|item| item.get("text").and_then(Value::as_str))
            })
    });
    let mut definitions = Vec::new();
    if let Some(meanings) = entry.get("meanings").and_then(Value::as_array) {
        for meaning in meanings {
            let part_of_speech = meaning
                .get("partOfSpeech")
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(items) = meaning.get("definitions").and_then(Value::as_array) {
                for item in items {
                    let Some(definition) = item.get("definition").and_then(Value::as_str) else {
                        continue;
                    };
                    if definition.trim().is_empty() {
                        continue;
                    }
                    let example = item
                        .get("example")
                        .and_then(Value::as_str)
                        .map(|text| clip(text, 180));
                    definitions.push(json!({
                        "part_of_speech":part_of_speech,
                        "definition":clip(definition, 350),
                        "example":example
                    }));
                    if definitions.len() == 5 {
                        break;
                    }
                }
            }
            if definitions.len() == 5 {
                break;
            }
        }
    }
    if definitions.is_empty() {
        return Err("Dictionary service returned no definitions".into());
    }
    Ok(json!({
        "word":word,
        "phonetic":phonetic,
        "definitions":definitions,
        "source":"Free Dictionary API",
        "source_url":source_url
    }))
}

pub async fn search_encyclopedia(client: &Client, query: &str) -> Result<Value, String> {
    let query = query.trim();
    if query.chars().count() < 2
        || query.chars().count() > 120
        || query.chars().any(char::is_control)
    {
        return Err("Provide a topic of 2 through 120 characters".into());
    }
    let response = client
        .get(WIKIPEDIA_API)
        .header(USER_AGENT, WIKIMEDIA_USER_AGENT)
        .query(&[
            ("action", "query"),
            ("generator", "search"),
            ("gsrsearch", query),
            ("gsrnamespace", "0"),
            ("gsrlimit", "3"),
            ("prop", "extracts"),
            ("exintro", "1"),
            ("explaintext", "1"),
            ("exchars", "400"),
            ("exlimit", "3"),
            ("format", "json"),
            ("formatversion", "2"),
        ])
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| "Wikipedia is unavailable right now")?;
    if !response.status().is_success() {
        return Err(format!(
            "Wikipedia returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let payload = response
        .json::<Value>()
        .await
        .map_err(|_| "Wikipedia returned invalid data")?;
    parse_encyclopedia(&payload, query)
}

fn parse_encyclopedia(payload: &Value, query: &str) -> Result<Value, String> {
    if payload.get("error").is_some() {
        return Err("Wikipedia could not complete the search".into());
    }
    let pages = payload
        .get("query")
        .and_then(|query| query.get("pages"))
        .and_then(Value::as_array)
        .ok_or("No encyclopedia articles found")?;
    let mut results = pages
        .iter()
        .filter_map(|page| {
            let title = page.get("title")?.as_str()?;
            let page_id = page.get("pageid")?.as_u64().filter(|id| *id > 0)?;
            let extract = page.get("extract")?.as_str()?.trim();
            if extract.is_empty() {
                return None;
            }
            Some((
                page.get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX),
                json!({
                    "title":title,
                    "extract":clip(extract, 600),
                    "source_url":format!("https://en.wikipedia.org/?curid={page_id}")
                }),
            ))
        })
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Err("No encyclopedia articles found".into());
    }
    results.sort_by_key(|(index, _)| *index);
    Ok(json!({
        "query":query,
        "source":"Wikipedia",
        "results":results.into_iter().map(|(_, result)| result).collect::<Vec<_>>()
    }))
}

fn clip(text: &str, max_chars: usize) -> String {
    text.trim().chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_returns_short_sourced_definitions() {
        assert_eq!(
            dictionary_url("ice cream").unwrap().as_str(),
            "https://api.dictionaryapi.dev/api/v2/entries/en/ice%20cream"
        );
        let payload = json!([{"word":"hello","phonetic":"həˈləʊ","meanings":[
            {"partOfSpeech":"exclamation","definitions":[
                {"definition":"Used as a greeting.","example":"Hello there!"},
                {"definition":"Used to answer the phone."}
            ]}
        ]}]);
        let result = parse_dictionary(&payload, "https://api.dictionaryapi.dev/example").unwrap();
        assert_eq!(result["word"], "hello");
        assert_eq!(
            result["definitions"][0]["definition"],
            "Used as a greeting."
        );
        assert_eq!(result["definitions"][1]["part_of_speech"], "exclamation");
        assert!(result["source_url"]
            .as_str()
            .unwrap()
            .starts_with("https://"));
        assert!(parse_dictionary(&json!([]), "url").is_err());
    }

    #[test]
    fn encyclopedia_keeps_search_order_and_article_links() {
        let payload = json!({"query":{"pages":[
            {"index":2,"pageid":22,"title":"Second","extract":"Second article."},
            {"index":1,"pageid":11,"title":"First","extract":"First article."},
            {"index":3,"pageid":33,"title":"Empty","extract":""}
        ]}});
        let result = parse_encyclopedia(&payload, "sample").unwrap();
        assert_eq!(result["results"].as_array().unwrap().len(), 2);
        assert_eq!(result["results"][0]["title"], "First");
        assert_eq!(
            result["results"][0]["source_url"],
            "https://en.wikipedia.org/?curid=11"
        );
        assert!(parse_encyclopedia(&json!({"query":{}}), "missing").is_err());
    }

    #[tokio::test]
    async fn invalid_queries_fail_before_network_requests() {
        let client = Client::new();
        assert!(define_word(&client, "word/../../bad").await.is_err());
        assert!(search_encyclopedia(&client, " ").await.is_err());
    }
}
