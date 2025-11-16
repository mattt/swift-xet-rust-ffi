use std::env;
use camino::Utf8PathBuf;
use uniffi_bindgen::bindings::SwiftBindingGenerator;

fn main() {
    let args: Vec<String> = env::args().collect();
    let udl_file = Utf8PathBuf::from(&args[1]);
    let out_dir = if args.len() > 2 {
        Utf8PathBuf::from(&args[2])
    } else {
        Utf8PathBuf::from(".")
    };
    
    println!("Generating Swift bindings from {:?} to {:?}", udl_file, out_dir);
    
    uniffi_bindgen::generate_bindings(
        &udl_file,
        None,
        SwiftBindingGenerator,
        Some(&out_dir),
        None,
        None,
        false,
    ).expect("Failed to generate bindings");
    
    println!("Swift bindings generated successfully!");
}
