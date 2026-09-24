pub(crate) mod global;
pub(crate) mod region;
pub(crate) mod local;

pub(crate) trait RelativeTimeRange {
    fn interval_duration(&self) -> u16;
    fn interval_offset(&self) -> u16;
}

pub(crate) trait RasterQuery {
    fn raster_baselength(&self) -> u16;
    fn count_threshold(&self) -> u8;
}

#[derive(Clone)]
pub struct RasterParameters {
    pub longitude_start: f64,
    pub longitude_delta: f64,
    pub longitude_bins: u16,
    pub latitude_start: f64,
    pub latitude_delta: f64,
    pub latitude_bins: u16,
}

impl RasterParameters {
    pub fn new(longitude_start: f64, longitude_delta: f64, longitude_bins:u16, latitude_start: f64, latitude_delta:f64, latitude_bins: u16) -> Self {
        Self {longitude_start, longitude_delta, longitude_bins, latitude_start, latitude_delta, latitude_bins }
    }
}

#[derive(Clone)]
pub struct RasterEntry {
    pub x: i16,
    pub y: i16,
    pub count: u16,
    pub time: u16,
}

#[derive(Clone)]
pub struct RasterData {
    pub parameters: RasterParameters,
    pub entries: Vec<RasterEntry>,
}

impl RasterData {
    pub fn new(parameters: RasterParameters, entries: Vec<RasterEntry>) -> Self {
        Self { parameters, entries }
    }
}
