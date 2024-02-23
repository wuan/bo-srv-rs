use chrono::NaiveDateTime;

pub mod config;

pub fn add(left: usize, right: usize) -> usize {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}

#[derive(Debug)]
pub struct Strike {
    pub timestamp: NaiveDateTime,
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: f64,
    pub amplitude: f64,
    pub lateral_error: i32,
    pub station_count: i32,
    pub station_ids: Vec<i32>,
}