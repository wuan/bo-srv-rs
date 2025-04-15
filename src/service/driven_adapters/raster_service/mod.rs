use crate::app::raster::model::{RasterData, RasterParameters};

use crate::app::driven_ports::raster_data::RasterDataService;
struct JsonRpcRasterService<'a> {
    service: Box<dyn RasterDataService + 'a>
}
