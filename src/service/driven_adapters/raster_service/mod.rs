use crate::app::driven_ports::raster_data::GlobalRasterDataService;
use std::sync::Arc;

struct JsonRpcRasterService {
    service: Arc<dyn GlobalRasterDataService>,
}
