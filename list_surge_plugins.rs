use streamlib::ClapScanner;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scanner = ClapScanner::new();
    let plugins = scanner.scan_file("/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap")?;
    
    println!("Found {} plugins in Surge XT Effects.clap:\n", plugins.len());
    for (i, plugin) in plugins.iter().enumerate() {
        println!("[{}] {}", i, plugin.name);
        println!("    ID: {}", plugin.id);
        println!("    Description: {}", plugin.description);
        println!();
    }
    
    Ok(())
}
