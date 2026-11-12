use atlas_core::search::{
    ensure_service_started, SearchBackend, SearchQuery, WindowsServiceBackend,
};

fn main() {
    println!("1. Ensuring search service is started...");
    let started = ensure_service_started();
    println!("   -> ensure_service_started returned: {started}");

    let backend = WindowsServiceBackend::new();
    println!("2. Checking backend status: {:#?}", backend.status());

    println!("3. Querying instant search for 'cargo'...");
    let mut query = SearchQuery::text("cargo");
    query.limit = 5;
    let batch = backend.search(&query);
    println!("   -> Found {} results:", batch.results.len());
    for result in batch.results {
        println!("      - {}", result.path.display());
    }
}
