use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use crate::app::driven_ports::raster_data::{
    GlobalRasterDataService, LocalRasterDataService, RegionRasterDataService,
};
use crate::app::raster::model::global::GlobalRasterQuery;
use crate::app::raster::model::local::LocalRasterQuery;
use crate::app::raster::model::region::RegionRasterQuery;
use crate::app::raster::model::RasterData;
use moka::future::Cache;
use std::sync::Arc;

struct LocalRasterDataServiceImpl {}

impl LocalRasterDataService for LocalRasterDataServiceImpl {
    async fn local(&mut self, params: LocalRasterQuery) -> RasterData {
        return RasterData
    }
}

struct GlobalRasterDataServiceImpl {
    cache: Cache<GlobalRasterQuery, RasterData>,
    service: Arc<dyn GlobalRasterDataService>,
}

impl GlobalRasterDataService for GlobalRasterDataServiceImpl {
    async fn global(&mut self, params: GlobalRasterQuery) -> &RasterData {

        let cache = Cache::new(10_000);
        let result = self.service.global(&params).await;
        let res = self
            .cache
            .entry(params)
            .or_insert_with(move || {
                let x = self.service.global(&params);
                x
            });
        res.await.value()
    }
}

struct RegionRasterDataServiceImpl {}

impl RegionRasterDataService for RegionRasterDataServiceImpl {
    async fn region(&mut self, params: Arc<dyn RegionRasterQuery>) -> RasterData {
        todo!()
    }
}
