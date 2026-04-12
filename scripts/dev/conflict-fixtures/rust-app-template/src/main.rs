use fixture_app::merge_engine::build_merge_plan;
use fixture_app::session::{default_session_defaults, status_label};

fn main() {
    let defaults = default_session_defaults();
    println!("session={}", status_label(&defaults));

    for step in build_merge_plan() {
        println!("plan={step}");
    }
}
