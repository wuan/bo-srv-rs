use crate::app::raster::model::{RasterQuery, RelativeTimeRange};


pub(crate) struct LocalRasterQuery {
    pub interval_duration: u16,
    pub interval_offset: u16,
    pub raster_baselength: u16,
    pub longitude_reference: u16,
    pub latitude_reference: u16,
    pub count_threshold: u8,
}

impl RelativeTimeRange for LocalRasterQuery {
    fn interval_duration(&self) -> u16 {
        self.interval_duration
    }

    fn interval_offset(&self) -> u16 {
        self.interval_offset
    }
}

impl RasterQuery for LocalRasterQuery {
    fn raster_baselength(&self) -> u16 {
        self.raster_baselength
    }

    fn count_threshold(&self) -> u8 {
        self.count_threshold
    }
}