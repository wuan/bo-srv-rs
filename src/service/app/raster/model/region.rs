use crate::app::raster::model::{RasterQuery, RelativeTimeRange};

pub(crate) trait RegionRasterQuery: RelativeTimeRange + RasterQuery {
    fn region(&self) -> u8;
}

pub struct RegionRasterQueryImpl {
    pub interval_duration: u16,
    pub interval_offset: u16,
    pub raster_baselength: u16,
    pub region: u8,
    pub count_threshold: u8,
}

impl RelativeTimeRange for RegionRasterQueryImpl {
    fn interval_duration(&self) -> u16 {
        self.interval_duration
    }

    fn interval_offset(&self) -> u16 {
        self.interval_offset
    }
}

impl RasterQuery for RegionRasterQueryImpl {
    fn raster_baselength(&self) -> u16 {
        self.raster_baselength
    }

    fn count_threshold(&self) -> u8 {
        self.count_threshold
    }
}

impl RegionRasterQuery for RegionRasterQueryImpl {
    fn region(&self) -> u8 {
        self.region
    }
}