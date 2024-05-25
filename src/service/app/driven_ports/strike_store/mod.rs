use crate::app::raster::model::RasterEntry;

pub trait StrikeStore {
    fn raster_data() -> Vec<RasterEntry>;
}