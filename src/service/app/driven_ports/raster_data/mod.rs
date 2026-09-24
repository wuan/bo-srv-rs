use crate::app::raster::model::global::GlobalRasterQuery;
use crate::app::raster::model::local::LocalRasterQuery;
use crate::app::raster::model::region::RegionRasterQuery;
use crate::app::raster::model::RasterData;
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub(crate) trait LocalRasterDataService {
    async fn local(&mut self, params: LocalRasterQuery) -> RasterData;
}

#[async_trait]
pub(crate) trait GlobalRasterDataService {
    async fn global(&mut self, params: &GlobalRasterQuery) -> RasterData;
}

#[async_trait]
pub(crate) trait RegionRasterDataService {
    async fn region(&mut self, params: Arc<dyn RegionRasterQuery>) -> RasterData;
}
