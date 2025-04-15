use async_trait::async_trait;
use crate::app::raster::model::{GlobalRasterQuery, LocalRasterQuery, RasterData, RegionRasterQuery};

#[async_trait]
pub(crate) trait RasterDataService {
    async fn local(&mut self, params: LocalRasterQuery) -> RasterData;
    async fn global(&mut self, params: Box<dyn GlobalRasterQuery>) -> RasterData;
    async fn region(&mut self, params: RegionRasterQuery) -> RasterData;
}