use crate::app::raster::model::{RasterQuery, RelativeTimeRange};

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

impl GlobalRasterQuery for GlobalRasterQueryImpl {

}
