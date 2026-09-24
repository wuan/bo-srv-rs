use crate::app::raster::model::{RasterQuery, RelativeTimeRange};


#[derive(Hash, Eq, PartialEq, Debug)]
pub struct GlobalRasterQuery {
    interval_duration: u16,
    interval_offset: u16,
    raster_baselength: u16,
    count_threshold: u8,
}

impl RelativeTimeRange for GlobalRasterQuery {
    fn interval_duration(&self) -> u16 {
        self.interval_duration
    }

    fn interval_offset(&self) -> u16 {
        self.interval_offset
    }
}

impl RasterQuery for GlobalRasterQuery {
    fn raster_baselength(&self) -> u16 {
        self.raster_baselength
    }

    fn count_threshold(&self) -> u8 {
        self.count_threshold
    }
}
