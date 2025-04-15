use std::sync::Arc;
use crate::app::raster::model::global::GlobalRasterQuery;
use crate::app::raster::model::local::LocalRasterQuery;
use crate::app::raster::model::RasterData;
use crate::app::raster::model::region::RegionRasterQuery;

pub(crate) trait RasterQuery {
    async fn local(params: Arc<dyn LocalRasterQuery>) -> RasterData;
    async fn global(params: Arc<dyn GlobalRasterQuery>) -> RasterData;
    async fn region(params: Arc<dyn RegionRasterQuery>) -> RasterData;
}