// Focused probe: on the Linux GTK3 backend, does DropDown::connect_changed
// actually fire when the selection changes?
use rswidgets::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = App::init()?;
    let dialog = app.new_dialog()?;
    dialog.set_title("changed-probe");
    dialog.set_default_size(300, 160);

    let dd = app.create_dropdown(&["Alpha", "Beta", "Gamma"])?;
    dd.set_active(Some(0));

    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<i32>::new()));
    let s2 = seen.clone();
    let dd2 = dd.clone();
    dd.connect_changed(move || {
        let a = dd2.get_active();
        s2.borrow_mut().push(a);
        println!("[changed] fired, get_active()={}", a);
    })?;
    println!("after connect: active={} seen={:?}", dd.get_active(), seen.borrow());
    println!("backend is_gtk4={}", dd.is_gtk4());
    let conn = dd.connect_changed(move || {});
    println!("connect_changed result: {:?}", conn);

    dialog.append_content_area(&dd);
    dialog.add_button("Close", -7);
    dialog.connect_response(move |_| {}).ok();
    dialog.present();

    // Programmatic change: does that notify?
    println!("-- set_active(2) (programmatic)");
    dd.set_active(Some(2));
    println!("   active={} seen={:?}", dd.get_active(), seen.borrow());

    println!("-- set_active(1) (programmatic)");
    dd.set_active(Some(1));
    println!("   active={} seen={:?}", dd.get_active(), seen.borrow());

    // Wait for a real popup selection driven by X, or 8s.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if seen.borrow().len() > 2 { break; }
    }
    println!("FINAL seen={:?}", seen.borrow());
    let _ = Box::into_raw(Box::new(dd));
    let _ = Box::into_raw(Box::new(dialog));
    Ok(())
}