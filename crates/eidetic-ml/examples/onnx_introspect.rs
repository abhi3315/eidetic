// Throwaway: print real ONNX input/output signatures so the YuNet decoder is
// written against measured tensor names/shapes rather than documentation.
use ort::session::Session;

fn dump(label: &str, path: &str) {
    println!("\n===== {label} =====");
    match Session::builder().and_then(|mut b| b.commit_from_file(path)) {
        Ok(s) => {
            println!("--- inputs ---");
            for i in s.inputs() {
                println!("  {:<12} {:?}", i.name(), i.dtype());
            }
            println!("--- outputs ---");
            for o in s.outputs() {
                println!("  {:<12} {:?}", o.name(), o.dtype());
            }
        }
        Err(e) => println!("  FAILED to load: {e}"),
    }
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: prog <model-dir>");
    dump("YuNet (detection)", &format!("{dir}/yunet.onnx"));
    dump("SFace (recognition)", &format!("{dir}/sface.onnx"));
}
