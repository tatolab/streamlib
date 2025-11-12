use objc2::MainThreadMarker;

#[tokio::main]
async fn main() {
    println!("Starting...");
    
    // Try to get MainThreadMarker
    match MainThreadMarker::new() {
        Some(mtm) => println!("✅ MainThreadMarker obtained - we are on main thread"),
        None => println!("❌ MainThreadMarker failed - we are NOT on main thread"),
    }
    
    println!("Done");
}
