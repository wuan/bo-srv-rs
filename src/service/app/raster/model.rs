pub struct GlobalRasterQuery {
    pub interval_duration: u16,
    pub interval_offset: u16,
    pub raster_baselength: u16,
    pub count_threshold: u8,
}

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
    pub raster_parameters: RasterParameters,

    pub entries: Vec<RasterEntry>,
}
