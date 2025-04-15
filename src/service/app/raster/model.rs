trait RelativeTimeRange {
    fn interval_duration(&self) -> u16;
    fn interval_offset(&self) -> u16;
}

trait RasterQuery {
    fn raster_baselength(&self) -> u16;
    fn count_threshold(&self) -> u8;
}

pub(crate) trait GlobalRasterQuery : RelativeTimeRange + RasterQuery {
}

pub struct GlobalRasterQueryImpl {
    interval_duration: u16,
    interval_offset: u16,
    raster_baselength: u16,
    count_threshold: u8,
}

impl RelativeTimeRange for GlobalRasterQueryImpl {
    fn interval_duration(&self) -> u16 {
        self.interval_duration
    }

    fn interval_offset(&self) -> u16 {
        self.interval_offset
    }
}

impl RasterQuery for GlobalRasterQueryImpl {
    fn raster_baselength(&self) -> u16 {
        self.raster_baselength
    }

    fn count_threshold(&self) -> u8 {
        self.count_threshold
    }
}

impl GlobalRasterQuery for GlobalRasterQueryImpl {}

pub struct RegionRasterQuery {
    pub interval_duration: u16,
    pub interval_offset: u16,
    pub raster_baselength: u16,
    pub region: u8,
    pub count_threshold: u8,
}

pub struct LocalRasterQuery {
    pub interval_duration: u16,
    pub interval_offset: u16,
    pub raster_baselength: u16,
    pub longitude_reference: u16,
    pub latitude_reference: u16,
    pub count_threshold: u8,
}

pub struct RasterParameters {
    pub longitude_start: f64,
    pub longitude_delta: f64,
    pub longitude_bins: u16,
    pub latitude_start: f64,
    pub latitude_delta: f64,
    pub latitude_bins: u16,
}

pub struct RasterEntry {
    pub x: i16,
    pub y: i16,
    pub count: u16,
    pub time: u16,
}

pub struct RasterData {
    pub parameters: RasterParameters,
    pub entries: Vec<RasterEntry>,
}
