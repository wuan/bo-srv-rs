use crate::app::raster::model::{GlobalRasterQuery, LocalRasterQuery, RasterData, RegionRasterQuery};


pub(crate) trait RasterQuery {
    async fn local(params: LocalRasterQuery) -> RasterData;
    async fn global(params: Box<dyn GlobalRasterQuery>) -> RasterData;
    async fn region(params: RegionRasterQuery) -> RasterData;
}