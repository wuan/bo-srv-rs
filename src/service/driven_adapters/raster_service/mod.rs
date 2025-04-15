use std::sync::Arc;
use crate::app::driven_ports::raster_data::RasterDataService;
struct JsonRpcRasterService {
    service: Arc<dyn RasterDataService>
}
