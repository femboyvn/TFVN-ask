//! Read-only tools backed by free public weather and quote APIs.

use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name":"weather",
            "description":"Look up current model-based weather and the next seven days for a city. Use for current weather, clothes, travel, meetings, events, and plans that bad weather could affect. If a meeting day and hour are known, pass them for an hour-specific forecast. Ask for a city if none is known; never guess the member's location.",
            "inputSchema":{"type":"object","properties":{
                "city":{"type":"string","minLength":2,"maxLength":100,"description":"City, optionally followed by country or region, e.g. Da Nang, Vietnam"},
                "day":{"type":"string","description":"Optional event day: today, tomorrow, or YYYY-MM-DD in the city's local time"},
                "hour":{"type":"integer","minimum":0,"maximum":23,"description":"Optional event hour in 24-hour local time; requires day"}
            },"required":["city"],"additionalProperties":false}
        }),
        json!({
            "name":"quote",
            "description":"Fetch a random or daily attributed quote from a free public API of public-domain book excerpts. The API cannot filter by topic and does not supply the original work.",
            "inputSchema":{"type":"object","properties":{
                "kind":{"type":"string","enum":["random","daily"],"description":"Use daily for quote of the day, otherwise random"}
            },"additionalProperties":false}
        }),
    ]
}

pub async fn call(client: &Client, name: &str, arguments: &Value) -> Result<Value, String> {
    match name {
        "weather" => {
            let object = arguments
                .as_object()
                .ok_or("Weather tool arguments must be an object")?;
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "city" | "day" | "hour"))
            {
                return Err("Unknown weather tool argument".into());
            }
            let city = arguments
                .get("city")
                .and_then(Value::as_str)
                .ok_or("Missing city")?;
            let day = match object.get("day") {
                Some(Value::String(day)) => Some(day.as_str()),
                None => None,
                _ => return Err("Weather day must be today, tomorrow, or YYYY-MM-DD".into()),
            };
            let hour = match object.get("hour") {
                Some(value) => Some(
                    value
                        .as_u64()
                        .filter(|hour| *hour <= 23)
                        .ok_or("Weather hour must be 0 through 23")? as u8,
                ),
                None => None,
            };
            if hour.is_some() && day.is_none() {
                return Err("Specify a day when asking for an event hour".into());
            }
            weather(client, city, day, hour).await
        }
        "quote" => {
            let object = arguments
                .as_object()
                .ok_or("Quote tool arguments must be an object")?;
            if object.keys().any(|key| key != "kind") {
                return Err(
                    "Quote API has no topic filter; request a random or daily quote".into(),
                );
            }
            let kind = match object.get("kind") {
                Some(Value::String(kind)) => kind.as_str(),
                None => "random",
                _ => return Err("Quote kind must be random or daily".into()),
            };
            quote(client, kind).await
        }
        _ => Err("Unknown skill tool".into()),
    }
}

async fn get_json(
    client: &Client,
    url: &str,
    parameters: &[(&str, String)],
    service: &str,
) -> Result<Value, String> {
    let response = client
        .get(url)
        .query(parameters)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| format!("{service} service is unavailable right now"))?;
    if !response.status().is_success() {
        return Err(format!(
            "{service} service returned HTTP {}",
            response.status().as_u16()
        ));
    }
    response
        .json()
        .await
        .map_err(|_| format!("{service} service returned invalid data"))
}

async fn weather(
    client: &Client,
    city: &str,
    day: Option<&str>,
    hour: Option<u8>,
) -> Result<Value, String> {
    let city = city.trim();
    if city.chars().count() < 2 || city.chars().count() > 100 || city.chars().any(char::is_control)
    {
        return Err("Please provide a city name between 2 and 100 characters".into());
    }
    let (search_name, qualifier) = city
        .split_once(',')
        .map(|(name, qualifier)| (name.trim(), Some(qualifier.trim())))
        .unwrap_or((city, None));
    if search_name.chars().count() < 2 || qualifier == Some("") {
        return Err("Please provide a city, optionally followed by a country or region".into());
    }
    let geocoding = get_json(
        client,
        "https://geocoding-api.open-meteo.com/v1/search",
        &[
            ("name", search_name.to_owned()),
            ("count", "20".into()),
            ("language", "en".into()),
        ],
        "Weather",
    )
    .await?;
    let results = geocoding
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("I couldn't find {city}. Try adding a country or region."))?;
    let place = select_place(results, qualifier)
        .ok_or_else(|| format!("I couldn't find {city}. Try another city or region."))?;
    let latitude = place
        .get("latitude")
        .and_then(Value::as_f64)
        .ok_or("Weather location has no latitude")?;
    let longitude = place
        .get("longitude")
        .and_then(Value::as_f64)
        .ok_or("Weather location has no longitude")?;
    let mut parameters = vec![
        ("latitude", latitude.to_string()),
        ("longitude", longitude.to_string()),
        ("current", "temperature_2m,relative_humidity_2m,apparent_temperature,precipitation,weather_code,wind_speed_10m".into()),
        ("daily", "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,wind_speed_10m_max".into()),
        ("forecast_days", "7".into()),
        ("timezone", "auto".into()),
    ];
    if hour.is_some() {
        parameters.push((
            "hourly",
            "temperature_2m,precipitation_probability,weather_code,wind_speed_10m".into(),
        ));
    }
    let forecast = get_json(
        client,
        "https://api.open-meteo.com/v1/forecast",
        &parameters,
        "Weather",
    )
    .await?;
    parse_weather(place, &forecast, day, hour)
}

fn select_place<'a>(results: &'a [Value], qualifier: Option<&str>) -> Option<&'a Value> {
    let Some(qualifier) = qualifier else {
        return results.first();
    };
    results.iter().find(|place| {
        ["country", "admin1", "admin2", "country_code"]
            .iter()
            .filter_map(|key| place.get(*key).and_then(Value::as_str))
            .any(|part| part.eq_ignore_ascii_case(qualifier))
    })
}

fn weather_description(code: i64) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 | 63 | 65 => "rain",
        66 | 67 => "freezing rain",
        71 | 73 | 75 | 77 => "snow",
        80..=82 => "rain showers",
        85 | 86 => "snow showers",
        95 | 96 | 99 => "thunderstorm",
        _ => "unknown conditions",
    }
}

fn daily_values<'a>(daily: &'a Value, key: &str) -> Option<&'a [Value]> {
    daily.get(key).and_then(Value::as_array).map(Vec::as_slice)
}

fn clothing_notes_today(
    feels_like_c: f64,
    code: i64,
    precipitation_mm: f64,
    rain_chance_percent: f64,
    wind_kmh: f64,
) -> Vec<&'static str> {
    let mut notes = vec![if feels_like_c >= 30.0 {
        "Choose light, breathable clothes."
    } else if feels_like_c >= 22.0 {
        "Light clothes should be comfortable."
    } else if feels_like_c >= 12.0 {
        "Bring a light jacket or layer."
    } else {
        "Wear warm layers and a coat."
    }];
    if precipitation_mm > 0.0
        || rain_chance_percent >= 50.0
        || matches!(code, 51..=67 | 80..=82 | 95 | 96 | 99)
    {
        notes.push("Bring an umbrella or a rain jacket.");
    }
    if wind_kmh >= 30.0 {
        notes.push("A wind-resistant outer layer may help.");
    }
    if matches!(code, 0 | 1) && feels_like_c >= 25.0 {
        notes.push("Consider a sun hat and sunscreen outdoors.");
    }
    notes
}

fn planning_note(
    code: i64,
    rain_chance: Option<f64>,
    wind_kmh: Option<f64>,
) -> Option<&'static str> {
    if matches!(code, 95 | 96 | 99) {
        Some("Thunderstorms are forecast; allow extra travel time and check local alerts.")
    } else if matches!(code, 56 | 57 | 66 | 67) {
        Some("Freezing precipitation is forecast; travel may be slippery.")
    } else if matches!(code, 71..=77 | 85 | 86) {
        Some("Snow is forecast; allow extra travel time.")
    } else if wind_kmh.is_some_and(|wind| wind >= 40.0) {
        Some("Strong wind is forecast; allow extra travel time and secure loose items.")
    } else if rain_chance.is_some_and(|chance| chance >= 70.0) {
        Some("Rain chance reaches at least 70%; allow extra travel time and bring rain protection.")
    } else if matches!(code, 51..=65 | 80..=82) || rain_chance.is_some_and(|chance| chance >= 50.0)
    {
        Some("Rain is possible; bring rain protection and allow extra travel time.")
    } else if matches!(code, 45 | 48) {
        Some("Fog is forecast; visibility may affect travel.")
    } else {
        None
    }
}

fn target_day_index(dates: &[Value], day: &str) -> Result<usize, String> {
    let target = match day {
        "today" => dates.first().and_then(Value::as_str),
        "tomorrow" => dates.get(1).and_then(Value::as_str),
        date if is_iso_date(date) => Some(date),
        _ => return Err("Weather day must be today, tomorrow, or YYYY-MM-DD".into()),
    }
    .ok_or("Weather service returned no dates")?;
    dates
        .iter()
        .position(|date| date.as_str() == Some(target))
        .ok_or("That day is outside the seven-day forecast")
        .map_err(str::to_owned)
}

fn is_iso_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn hourly_value<'a>(hourly: &'a Value, key: &str, index: usize) -> Option<&'a Value> {
    hourly.get(key).and_then(Value::as_array)?.get(index)
}

fn parse_weather(
    place: &Value,
    forecast: &Value,
    day: Option<&str>,
    hour: Option<u8>,
) -> Result<Value, String> {
    let name = place
        .get("name")
        .and_then(Value::as_str)
        .ok_or("Weather location has no name")?;
    let country = place.get("country").and_then(Value::as_str).unwrap_or("");
    let admin = place.get("admin1").and_then(Value::as_str).unwrap_or("");
    let mut parts = vec![name];
    for part in [admin, country] {
        if !part.is_empty() && !parts.contains(&part) {
            parts.push(part);
        }
    }
    let location = parts.join(", ");
    let current = forecast
        .get("current")
        .and_then(Value::as_object)
        .ok_or("Weather service returned no current conditions")?;
    let temperature = current
        .get("temperature_2m")
        .and_then(Value::as_f64)
        .ok_or("Weather service returned no temperature")?;
    let code = current
        .get("weather_code")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let daily = forecast.get("daily").unwrap_or(&Value::Null);
    let dates = daily_values(daily, "time").unwrap_or(&[]);
    let feels_like = current
        .get("apparent_temperature")
        .and_then(Value::as_f64)
        .unwrap_or(temperature);
    let rain_chance = daily_values(daily, "precipitation_probability_max")
        .and_then(|items| items.first())
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let precipitation = current
        .get("precipitation")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let wind = current
        .get("wind_speed_10m")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let mut days = Vec::new();
    for (index, date) in dates.iter().take(7).enumerate() {
        let code = daily_values(daily, "weather_code")
            .and_then(|items| items.get(index))
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let rain_chance = daily_values(daily, "precipitation_probability_max")
            .and_then(|items| items.get(index))
            .and_then(Value::as_f64);
        let wind = daily_values(daily, "wind_speed_10m_max")
            .and_then(|items| items.get(index))
            .and_then(Value::as_f64);
        days.push(json!({
            "date":date,
            "condition":weather_description(code),
            "high_c":daily_values(daily, "temperature_2m_max").and_then(|items| items.get(index)),
            "low_c":daily_values(daily, "temperature_2m_min").and_then(|items| items.get(index)),
            "precipitation_chance_percent":rain_chance,
            "max_wind_kmh":wind,
            "planning_note":planning_note(code, rain_chance, wind)
        }));
    }
    let focused_index = day.map(|day| target_day_index(dates, day)).transpose()?;
    let focused_day = focused_index.and_then(|index| days.get(index)).cloned();
    let event_hour = if let (Some(index), Some(hour)) = (focused_index, hour) {
        let date = dates[index]
            .as_str()
            .ok_or("Weather service returned no date")?;
        let time = format!("{date}T{hour:02}:00");
        if current
            .get("time")
            .and_then(Value::as_str)
            .is_some_and(|now| time.as_str() < now)
        {
            return Err("The requested meeting hour has already passed locally".into());
        }
        let hourly = forecast
            .get("hourly")
            .ok_or("Weather service returned no hourly forecast")?;
        let hour_index = hourly
            .get("time")
            .and_then(Value::as_array)
            .and_then(|times| {
                times
                    .iter()
                    .position(|item| item.as_str() == Some(time.as_str()))
            })
            .ok_or("Weather service returned no forecast for that hour")?;
        let code = hourly_value(hourly, "weather_code", hour_index)
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let rain_chance =
            hourly_value(hourly, "precipitation_probability", hour_index).and_then(Value::as_f64);
        let wind = hourly_value(hourly, "wind_speed_10m", hour_index).and_then(Value::as_f64);
        Some(json!({
            "time_local":time,
            "condition":weather_description(code),
            "temperature_c":hourly_value(hourly, "temperature_2m", hour_index),
            "precipitation_chance_percent":rain_chance,
            "wind_kmh":wind,
            "planning_note":planning_note(code, rain_chance, wind)
        }))
    } else {
        None
    };
    Ok(json!({
        "location":location,
        "time_local":current.get("time"),
        "timezone":forecast.get("timezone"),
        "condition":weather_description(code),
        "temperature_c":temperature,
        "feels_like_c":current.get("apparent_temperature"),
        "humidity_percent":current.get("relative_humidity_2m"),
        "wind_kmh":current.get("wind_speed_10m"),
        "precipitation_mm":current.get("precipitation"),
        "clothing_notes_today":clothing_notes_today(feels_like, code, precipitation, rain_chance, wind),
        "forecast":days,
        "focused_day":focused_day,
        "event_hour":event_hour,
        "forecast_note":"Model forecast, not an official severe-weather warning. Check local alerts for safety-critical plans.",
        "source":"Open-Meteo weather models",
        "source_url":"https://open-meteo.com/"
    }))
}

async fn quote(client: &Client, kind: &str) -> Result<Value, String> {
    let endpoint = match kind {
        "random" => "https://dumbapis.com/quote",
        "daily" => "https://dumbapis.com/daily/quote",
        _ => return Err("Unsupported quote kind".into()),
    };
    let payload = get_json(client, endpoint, &[], "Quote").await?;
    parse_quote(&payload, kind, endpoint)
}

fn parse_quote(payload: &Value, kind: &str, endpoint: &str) -> Result<Value, String> {
    let text = payload
        .get("quote")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty() && text.chars().count() <= 800)
        .ok_or("Quote service returned no usable quote")?;
    let author = payload
        .get("author")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|author| !author.is_empty())
        .ok_or("Quote service returned no author")?;
    let mut result = json!({
        "text": text,
        "author": author,
        "source": "Dumb APIs public-domain book excerpts",
        "source_url": endpoint
    });
    if kind == "daily" {
        result["date_utc"] = payload
            .get("date")
            .cloned()
            .ok_or("Quote service returned no date")?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_weather_and_forecast() {
        let place = json!({"name":"Da Nang","country":"Vietnam","latitude":16.0,"longitude":108.0});
        let forecast = json!({"timezone":"Asia/Ho_Chi_Minh","current":{"time":"2026-10-07T11:00","temperature_2m":30.2,"weather_code":2,"apparent_temperature":34.0,"relative_humidity_2m":77,"wind_speed_10m":10.0,"precipitation":0},"daily":{"time":["2026-10-07"],"weather_code":[61],"temperature_2m_max":[31.0],"temperature_2m_min":[25.0],"precipitation_probability_max":[70]}});
        let result = parse_weather(&place, &forecast, None, None).unwrap();
        assert_eq!(result["location"], "Da Nang, Vietnam");
        assert_eq!(result["temperature_c"], 30.2);
        assert_eq!(result["forecast"][0]["condition"], "rain");
        assert_eq!(
            result["forecast"][0]["precipitation_chance_percent"].as_f64(),
            Some(70.0)
        );
        assert!(result["clothing_notes_today"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("rain jacket")));
    }

    #[test]
    fn meeting_hour_flags_storms_in_local_forecast() {
        let place = json!({"name":"Da Nang","country":"Vietnam"});
        let forecast = json!({
            "timezone":"Asia/Ho_Chi_Minh",
            "current":{"time":"2026-10-07T11:00","temperature_2m":30.0,"weather_code":2},
            "daily":{"time":["2026-10-07","2026-10-08"],"weather_code":[2,95],"temperature_2m_max":[31,29],"temperature_2m_min":[25,24],"precipitation_probability_max":[20,90],"wind_speed_10m_max":[15,30]},
            "hourly":{"time":["2026-10-08T15:00"],"weather_code":[95],"temperature_2m":[28],"precipitation_probability":[85],"wind_speed_10m":[25]}
        });
        let result = parse_weather(&place, &forecast, Some("tomorrow"), Some(15)).unwrap();
        assert_eq!(result["focused_day"]["date"], "2026-10-08");
        assert_eq!(result["event_hour"]["time_local"], "2026-10-08T15:00");
        assert_eq!(result["event_hour"]["condition"], "thunderstorm");
        assert!(result["event_hour"]["planning_note"]
            .as_str()
            .unwrap()
            .contains("travel time"));
        assert!(parse_weather(&place, &forecast, Some("2026-11-01"), None).is_err());
    }

    #[test]
    fn parses_public_quote_api_payload() {
        let selected = parse_quote(
            &json!({"quote":"A short line.","author":"An Author","date":"2026-10-07"}),
            "daily",
            "https://dumbapis.com/daily/quote",
        )
        .unwrap();
        assert_eq!(selected["text"], "A short line.");
        assert_eq!(selected["author"], "An Author");
        assert_eq!(selected["date_utc"], "2026-10-07");
        assert!(parse_quote(&json!({"quote":"","author":"A"}), "random", "url").is_err());
    }

    #[tokio::test]
    async fn quote_rejects_unsupported_topic_without_an_api_call() {
        let result = call(&Client::new(), "quote", &json!({"topic":"hope"})).await;
        assert!(result.unwrap_err().contains("no topic filter"));
    }

    #[test]
    fn city_qualifier_selects_the_matching_country() {
        let results = json!([
            {"name":"Springfield","country":"United States","admin1":"Illinois"},
            {"name":"Springfield","country":"Australia","admin1":"Queensland"}
        ]);
        assert_eq!(
            select_place(results.as_array().unwrap(), Some("Australia")).unwrap()["country"],
            "Australia"
        );
        assert_eq!(
            select_place(results.as_array().unwrap(), Some("Queensland")).unwrap()["country"],
            "Australia"
        );
        assert!(select_place(results.as_array().unwrap(), Some("Vietnam")).is_none());
    }
}
