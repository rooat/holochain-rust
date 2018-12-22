use holochain_container_api::{
    config::{load_configuration, Configuration},
    container::Container,
};
use holochain_core::{
    signal::{signal_channel},
};
use holochain_core_types::{
    cas::content::Address,
    dna::{capabilities::CapabilityCall},
};
use neon::{context::Context, prelude::*};
use std::{
    sync::{
        mpsc::{sync_channel, SyncSender},
        Arc, Mutex,
    },
};

use crate::{
    config::{js_make_config},
    waiter::{CallBlockingTask, ControlMsg, MainBackgroundTask},
};

pub struct Habitat {
    container: Container,
    sender_tx: SyncSender<SyncSender<ControlMsg>>,
    is_running: Arc<Mutex<bool>>,
}

fn signal_callback(mut cx: FunctionContext) -> JsResult<JsNull> {
    println!("Background task shut down");
    Ok(cx.null())
}


declare_types! {

    /// A Habitat can be initialized either by:
    /// - an Object representation of a Configuration struct
    /// - a string representing TOML
    pub class JsHabitat for Habitat {
        init(mut cx) {
            let config_arg: Handle<JsValue> = cx.argument(0)?;
            let config: Configuration = if config_arg.is_a::<JsObject>() {
                neon_serde::from_value(&mut cx, config_arg)?
            } else if config_arg.is_a::<JsString>() {
                let toml_str: String = neon_serde::from_value(&mut cx, config_arg)?;
                load_configuration(&toml_str).expect("Could not load TOML config")
            } else {
                panic!("Invalid type specified for config, must be object or string");
            };
            let mut container = Container::from_config(config);

            let js_callback: Handle<JsFunction> = JsFunction::new(&mut cx, signal_callback)
                .unwrap()
                .as_value(&mut cx)
                .downcast_or_throw(&mut cx)
                .unwrap();
            let (signal_tx, signal_rx) = signal_channel();
            let (sender_tx, sender_rx) = sync_channel(1);
            let is_running = Arc::new(Mutex::new(true));
            let background_task = MainBackgroundTask::new(signal_rx, sender_rx, is_running.clone());
            background_task.schedule(js_callback);

            container.load_config_with_signal(Some(signal_tx)).or_else(|e| {
                let error_string = cx.string(format!("unable to initialize habitat: {}", e));
                cx.throw(error_string)
            })?;

            Ok(Habitat { container, sender_tx, is_running })
        }

        method start(mut cx) {
            let mut this = cx.this();

            let start_result: Result<(), String> = {
                let guard = cx.lock();
                let hab = &mut *this.borrow_mut(&guard);
                hab.container.start_all_instances().map_err(|e| e.to_string())
            };

            start_result.or_else(|e| {
                let error_string = cx.string(format!("unable to start habitat: {}", e));
                cx.throw(error_string)
            })?;

            Ok(cx.undefined().upcast())
        }

        method stop(mut cx) {
            let mut this = cx.this();

            let stop_result: Result<(), String> = {
                let guard = cx.lock();
                let hab = &mut *this.borrow_mut(&guard);
                let result = hab.container.stop_all_instances().map_err(|e| e.to_string());
                let mut is_running = hab.is_running.lock().unwrap();
                *is_running = false;
                result
            };

            stop_result.or_else(|e| {
                let error_string = cx.string(format!("unable to stop habitat: {}", e));
                cx.throw(error_string)
            })?;

            Ok(cx.undefined().upcast())
        }

        method call(mut cx) {
            let instance_id = cx.argument::<JsString>(0)?.to_string(&mut cx)?.value();
            let zome = cx.argument::<JsString>(1)?.to_string(&mut cx)?.value();
            let cap_name = cx.argument::<JsString>(2)?.to_string(&mut cx)?.value();
            let fn_name = cx.argument::<JsString>(3)?.to_string(&mut cx)?.value();
            let params = cx.argument::<JsString>(4)?.to_string(&mut cx)?.value();
            // let maybe_task_id = cx.argument_opt(5);

            let mut this = cx.this();

            let call_result = {
                let guard = cx.lock();
                let hab = &mut *this.borrow_mut(&guard);
                let cap = Some(CapabilityCall::new(
                    cap_name.to_string(),
                    Address::from(""), //FIXME
                    None,
                ));
                let instance_arc = hab.container.instances().get(&instance_id)
                    .expect(&format!("No instance with id: {}", instance_id));
                let mut instance = instance_arc.write().unwrap();
                instance.call(&zome, cap, &fn_name, &params)
            };

            let res_string = call_result.or_else(|e| {
                let error_string = cx.string(format!("unable to call zome function: {:?}", &e));
                cx.throw(error_string)
            })?;

            let result_string: String = res_string.into();

            // let completion_callback =
            Ok(cx.string(result_string).upcast())
        }

        method register_callback(mut cx) {
            let js_callback: Handle<JsFunction> = cx.argument(0)?;
            let this = cx.this();
            {
                let guard = cx.lock();
                let hab = &*this.borrow(&guard);
                
                let (tx, rx) = sync_channel(0);
                let task = CallBlockingTask { rx };
                task.schedule(js_callback);
                hab.sender_tx.send(tx).expect("Could not send to sender channel");
            }
            Ok(cx.undefined().upcast())
        }

        method agent_id(mut cx) {
            let instance_id = cx.argument::<JsString>(0)?.to_string(&mut cx)?.value();
            let this = cx.this();
            let result = {
                let guard = cx.lock();
                let hab = this.borrow(&guard);
                let instance = hab.container.instances().get(&instance_id)
                    .expect(&format!("No instance with id: {}", instance_id))
                    .read().unwrap();
                let out = instance.context().state().ok_or("No state?".to_string())
                    .and_then(|state| state
                        .agent().get_agent_address()
                        .map_err(|e| e.to_string()));
                out
            };

            let hash = result.or_else(|e: String| {
                let error_string = cx.string(format!("unable to call zome function: {:?}", &e));
                cx.throw(error_string)
            })?;
            Ok(cx.string(hash.to_string()).upcast())
        }
    }
}

register_module!(mut m, {
    m.export_function("makeConfig", js_make_config)?;
    m.export_class::<JsHabitat>("Habitat")?;
    Ok(())
});
