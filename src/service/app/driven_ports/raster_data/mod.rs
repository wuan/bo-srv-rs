use std::sync::Arc;
use async_trait::async_trait;
use crate::app::raster::model::global::GlobalRasterQuery;
use crate::app::raster::model::local::LocalRasterQuery;
use crate::app::raster::model::RasterData;
use crate::app::raster::model::region::RegionRasterQuery;

#[async_trait]
pub(crate) trait RasterDataService {
    async fn local(&mut self, params: Arc<dyn LocalRasterQuery>) -> RasterData;
    async fn global(&mut self, params: Arc<dyn GlobalRasterQuery>) -> RasterData;
    async fn region(&mut self, params: Arc<dyn RegionRasterQuery>) -> RasterData;
}