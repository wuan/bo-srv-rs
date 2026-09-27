use blitzortung_srv::geom::LocalGrid;
use blitzortung_srv::query::{grid_query, TimeInterval};
use chrono::{Duration, Utc};

fn main() {
    for (data_area, x, y) in [(5, 2, 9), (5, 16, 10), (10, 3, 5)] {
        let lg = LocalGrid { data_area, x, y };
        let grid = lg.grid_factory().get_for(5000.0);
        let now = Utc::now();
        let interval = TimeInterval::new(now - Duration::minutes(60), now);
        let q = grid_query(&grid, &interval, None, 0);
        println!("--- LocalGrid data_area={data_area} x={x} y={y} ---");
        println!(
            "bounds lon=[{:.8},{:.8}] lat=[{:.8},{:.8}] div=({:.8},{:.8})",
            grid.x_min, grid.x_max, grid.y_min, grid.y_max, grid.x_div, grid.y_div
        );
        println!("SQL: {}", q.to_postgres());
        println!("PARAMS: {:?}", q.parameters());
    }
}
