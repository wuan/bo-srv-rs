use crate::app::raster::model::{RasterData, RasterParameters};

trait RasterQuery {
    async fn get_raster(query: RasterParameters) -> RasterData;
}