use chrono::{DurationRound, NaiveDateTime};

use bo_srv_rs::config::read_config;

fn parse_line(region: i32, line: &str) -> Option<bo_srv_rs::Strike> {
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

    Some(bo_srv_rs::Strike {
        timestamp,
        latitude,
        longitude,
        altitude,
        amplitude,
        region: Some(region),
        lateral_error,
        station_count,
    })
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDateTime;

    use crate::parse_line;

    #[test]
    fn test_parse() -> Result<(), &'static str> {
        const LINE: &str = "2024-02-23 19:20:09.760253765 pos;37.931022;-69.886960;123 str;340 dev;6799 sta;14;32;2445,946,2297,2622,1933,1822";

        let result = parse_line(2, LINE).unwrap();

        assert_eq!(result.timestamp, NaiveDateTime::parse_from_str("2024-02-23 19:20:09.760253765", "%Y-%m-%d %H:%M:%S%.f").unwrap());
        assert_eq!(result.latitude, 37.931022);
        assert_eq!(result.longitude, -69.886960);
        assert_eq!(result.altitude, 123.0);
        assert_eq!(result.amplitude, 340.0);
        assert_eq!(result.lateral_error, 6799);
        assert_eq!(result.station_count, 14);
        assert_eq!(result.region, Some(2));

        Ok(())
    }
}

fn main() {
    let time_granularity = chrono::Duration::minutes(10);
    let config = read_config();
    let blitzortung = config.blitzortung;

    let region = 1;
    let base_url = "https://data.blitzortung.org/Data/Protected/Strikes_".to_owned() + &region.to_string() + "/";
    let now = chrono::Utc::now();
    let start_time = now - chrono::Duration::minutes(22);
    let mut time_block = start_time.duration_trunc(time_granularity).unwrap();

    while time_block < now {
        let path = time_block.format("%Y/%m/%d/%H/%M.log").to_string();

        let url = base_url.clone() + &path;
        println!("get {}", &url);
        let response = reqwest::blocking::Client::new()
            .get(url)
            .basic_auth(&blitzortung.username, Some(&blitzortung.password))
            .send()
            .unwrap();

        let body = response.text().unwrap();
        for line in body.split('\n') {
            if let Some(strike) = parse_line(region, line) {
                println!("{:?}", strike);
            }
        }
        time_block = time_block + time_granularity;
    }
}