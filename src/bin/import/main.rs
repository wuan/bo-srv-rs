use std::env;

use chrono::{DurationRound, NaiveDateTime};

use bo_srv_rs::config;

fn parse_line(line: &str) -> Option<bo_srv_rs::Strike> {
    let parts: Vec<&str> = line.split(' ').collect();
    if parts.len() != 6 {
        return None;
    }

    let mut date = parts[0].to_owned();
    date.push(' ');
    date.push_str(parts[1]);
    let timestamp = NaiveDateTime::parse_from_str(&date, "%Y-%m-%d %H:%M:%S%.f").ok()?;

    let mut pos_parts = parts[2].split(';');
    let latitude = pos_parts.nth(1).unwrap().parse::<f64>().ok()?;
    let longitude = pos_parts.next().unwrap().parse::<f64>().ok()?;
    let altitude = pos_parts.next().unwrap().parse::<f64>().ok()?;

    let mut str_parts = parts[3].split(';');
    let amplitude = str_parts.nth(1).unwrap().parse::<f64>().ok()?;

    let mut dev_parts = parts[4].split(';');
    let lateral_error = dev_parts.nth(1).unwrap().parse::<i32>().ok()?;

    let mut sta_parts = parts[5].split(';');
    let station_count = sta_parts.nth(1).unwrap().parse::<i32>().ok()?;
    let station_ids = sta_parts.nth(1).unwrap().split(',').map(|s| s.parse()).filter(|x| x.is_ok()).map(|x| x.unwrap()).collect();

    Some(bo_srv_rs::Strike {
        timestamp,
        latitude,
        longitude,
        altitude,
        amplitude,
        lateral_error,
        station_count,
        station_ids,
    })
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDateTime;

    use crate::parse_line;

    #[test]
    fn test_parse() -> Result<(), &'static str> {
        const LINE: &str = "2024-02-23 19:20:09.760253765 pos;37.931022;-69.886960;123 str;340 dev;6799 sta;14;32;2445,946,2297,2622,1933,1822";

        let result = parse_line(LINE).unwrap();

        assert_eq!(result.timestamp, NaiveDateTime::parse_from_str("2024-02-23 19:20:09.760253765", "%Y-%m-%d %H:%M:%S%.f").unwrap());
        assert_eq!(result.latitude, 37.931022);
        assert_eq!(result.longitude, -69.886960);
        assert_eq!(result.altitude, 123.0);
        assert_eq!(result.amplitude, 340.0);
        assert_eq!(result.lateral_error, 6799);
        assert_eq!(result.station_count, 14);
        assert_eq!(result.station_ids, vec![2445, 946, 2297, 2622, 1933, 1822]);

        Ok(())
    }
}

fn main() {
    let username = env::var("BO_USERNAME").unwrap();
    let password = env::var("BO_PASSWORD").unwrap();
    let creds = config::Credentials::new(username, password);

    let region = 1;
    let base_url = "https://data.blitzortung.org/Data/Protected/Strikes_".to_owned() + &region.to_string() + "/";
    let now = chrono::Utc::now();
    let time = now - chrono::Duration::minutes(2);
    let rounded = time.duration_trunc(chrono::Duration::minutes(10)).unwrap();
    let path = rounded.format("%Y/%m/%d/%H/%M.log").to_string();

    let url = base_url + &path;
    println!("get {}", &url);
    let response = reqwest::blocking::Client::new()
        .get(url)
        .basic_auth(&creds.username, Some(&creds.password))
        .send()
        .unwrap();

    let body = response.text().unwrap();
    for line in body.split('\n') {
        if let Some(strike) = parse_line(line) {
            println!("{} {},{}", strike.timestamp, strike.longitude, strike.latitude);
        }
    }
}