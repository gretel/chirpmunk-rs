// SPDX-License-Identifier: GPL-3.0-only

//! Chirpmunk frontend — server-driven prophecy GUI.
//!
//! Mounted in the browser by Trunk. Talks to chirpmunk-trx over the
//! FutureSDR ControlPort (`window.location.origin`) and to the
//! spectrum WebSocket (`ws://host:9001`).
//!
//! Adapted from `FutureSDR/examples/spectrum/src/wasm/frontend.rs`.
//! The chirpmunk flowgraph does not expose SDR knobs (freq, gain,
//! sample-rate) on its blocks the way that example assumes, so those
//! sliders / list-selectors are dropped. The `<Waterfall/>`,
//! `<TimeSink/>`, `<FlowgraphCanvas/>`, `<FlowgraphTable/>` and
//! `<PmtEditor/>` components are reused as-is from prophecy.

use futuresdr::futures::StreamExt;
use futuresdr::runtime::FlowgraphId;
use futuresdr::runtime::Pmt;
use gloo_net::websocket::Message;
use gloo_net::websocket::futures::WebSocket;
use prophecy::FlowgraphCanvas;
use prophecy::FlowgraphHandle;
use prophecy::FlowgraphTable;
use prophecy::PmtEditor;
use prophecy::RuntimeHandle;
use prophecy::TimeSink;
use prophecy::TimeSinkMode;
use prophecy::Waterfall;
use prophecy::WaterfallMode;
use prophecy::leptos::html::Span;
use prophecy::leptos::logging::*;
use prophecy::leptos::prelude::*;
use prophecy::leptos::task::spawn_local;
use prophecy::leptos::wasm_bindgen::JsCast;
use prophecy::leptos::web_sys::HtmlInputElement;
use prophecy::leptos::web_sys::KeyboardEvent;

const SPECTRUM_WS_PORT: u16 = 9001;

#[derive(Clone, Debug, PartialEq)]
struct MessageInputTarget {
    block_id: usize,
    block_name: String,
    handler: String,
    source: &'static str,
}

#[component]
/// Spectrum + flowgraph view bound to a chirpmunk-trx FlowgraphHandle.
pub fn Spectrum(fg_handle: FlowgraphHandle) -> impl IntoView {
    let fg_desc = LocalResource::new({
        let fg_handle = fg_handle.clone();
        move || {
            let fg_handle = fg_handle.clone();
            async move { fg_handle.describe().await.ok() }
        }
    });

    let (time_data, set_time_data) = signal(vec![]);
    let (waterfall_data, set_waterfall_data) = signal(vec![]);

    let ws_url = {
        let proto = window().location().protocol().unwrap();
        let host = window().location().hostname().unwrap();
        if proto == "http:" {
            format!("ws://{host}:{SPECTRUM_WS_PORT}")
        } else {
            format!("wss://{host}:{SPECTRUM_WS_PORT}")
        }
    };
    spawn_local(async move {
        let mut ws = match WebSocket::open(&ws_url) {
            Ok(ws) => ws,
            Err(e) => {
                log!("chirpmunk: cannot open spectrum WebSocket {ws_url}: {e:?}");
                return;
            }
        };
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Bytes(b)) => {
                    set_time_data(b.clone());
                    set_waterfall_data(b);
                }
                _ => {
                    log!("chirpmunk: spectrum WS event {msg:?}");
                }
            }
        }
        log!("chirpmunk: spectrum WS closed");
    });

    let (min, set_min) = signal(-40.0f32);
    let (max, set_max) = signal(20.0f32);
    let min_label = NodeRef::<Span>::new();
    let max_label = NodeRef::<Span>::new();

    let (target, set_target) = signal(None::<MessageInputTarget>);
    let (submit_error, set_submit_error) = signal(None::<String>);
    let (submitting, set_submitting) = signal(false);
    let _esc_listener = window_event_listener(
        prophecy::leptos::ev::keydown,
        move |ev: KeyboardEvent| {
            if ev.key() == "Escape" && target.get_untracked().is_some() {
                set_target(None);
            }
        },
    );
    let on_canvas_message_input_click =
        Callback::new(move |(block_id, block_name, handler)| {
            set_submit_error(None);
            set_target(Some(MessageInputTarget {
                block_id,
                block_name,
                handler,
                source: "canvas",
            }));
        });
    let on_table_message_input_click =
        Callback::new(move |(block_id, block_name, handler)| {
            set_submit_error(None);
            set_target(Some(MessageInputTarget {
                block_id,
                block_name,
                handler,
                source: "table",
            }));
        });
    let fg_for_submit = fg_handle.clone();
    let on_submit_pmt = Callback::new(move |pmt: Pmt| {
        if let Some(selected) = target.get_untracked() {
            set_submitting(true);
            set_submit_error(None);
            let fg = fg_for_submit.clone();
            spawn_local(async move {
                let result = fg
                    .put_message_input(selected.block_id, selected.handler.clone(), pmt)
                    .await;
                set_submitting(false);
                match result {
                    Ok(()) => set_target(None),
                    Err(e) => set_submit_error(Some(format!("failed to send PMT: {e}"))),
                }
            });
        }
    });

    view! {
        <div class="bg-slate-800 border border-slate-700 rounded-xl m-4 p-5 shadow-lg">
            <div class="flex items-center justify-between mb-4">
                <h2 class="text-white text-lg font-semibold">"Power Scale"</h2>
                <span class="text-xs text-slate-400">
                    {format!("WS ws://{{host}}:{SPECTRUM_WS_PORT}")}
                </span>
            </div>
            <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
                <div class="bg-slate-900 border border-slate-700 rounded-lg p-3 flex flex-col">
                    <div class="text-slate-300 text-sm mb-2">"Min (dB)"</div>
                    <input
                        type="range"
                        min="-100"
                        max="50"
                        value="-40"
                        class="w-full align-middle accent-cyan-400"
                        on:change=move |v| {
                            let target = v.target().unwrap();
                            let input: HtmlInputElement = target.dyn_into().unwrap();
                            min_label
                                .get()
                                .unwrap()
                                .set_inner_text(&format!("min: {} dB", input.value()));
                            set_min(input.value().parse().unwrap());
                        }
                    />
                    <span class="text-slate-100 text-sm block mt-2" node_ref=min_label>"min: -40 dB"</span>
                </div>
                <div class="bg-slate-900 border border-slate-700 rounded-lg p-3 flex flex-col">
                    <div class="text-slate-300 text-sm mb-2">"Max (dB)"</div>
                    <input
                        type="range"
                        min="-40"
                        max="100"
                        value="20"
                        class="w-full align-middle accent-cyan-400"
                        on:change=move |v| {
                            let target = v.target().unwrap();
                            let input: HtmlInputElement = target.dyn_into().unwrap();
                            max_label
                                .get()
                                .unwrap()
                                .set_inner_text(&format!("max: {} dB", input.value()));
                            set_max(input.value().parse().unwrap());
                        }
                    />
                    <span class="text-slate-100 text-sm block mt-2" node_ref=max_label>"max: 20 dB"</span>
                </div>
            </div>
        </div>

        <div class="bg-slate-800 border border-slate-700 rounded-xl m-4 p-4 shadow-lg">
            <h2 class="text-white text-lg font-semibold mb-3">"Spectrum"</h2>
            <div class="border border-slate-700 rounded-lg" style="height: 400px; max-height: 40vh">
                <TimeSink min=min max=max mode=TimeSinkMode::Data(time_data) />
            </div>
        </div>

        <div class="bg-slate-800 border border-slate-700 rounded-xl m-4 p-4 shadow-lg">
            <h2 class="text-white text-lg font-semibold mb-3">"Waterfall"</h2>
            <div class="border border-slate-700 rounded-lg" style="height: 400px; max-height: 40vh">
                <Waterfall min=min max=max mode=WaterfallMode::Data(waterfall_data) />
            </div>
        </div>

        <div class="bg-slate-800 border border-slate-700 rounded-xl m-4 p-4 shadow-lg space-y-4">
            <h2 class="text-white text-lg font-semibold">"Flowgraph"</h2>
            {move || {
                if let Some(Some(desc)) = fg_desc.get() {
                    return view! {
                        <div class="border border-slate-700 rounded-lg">
                            <FlowgraphCanvas
                                fg=desc.clone()
                                on_message_input_click=on_canvas_message_input_click
                            />
                        </div>
                        <div class="border border-slate-700 rounded-lg overflow-x-auto">
                            <FlowgraphTable fg=desc on_message_input_click=on_table_message_input_click />
                        </div>
                    }
                        .into_any();
                }
                view! { <div class="text-slate-400 text-sm">"Loading flowgraph..."</div> }.into_any()
            }}
        </div>

        {move || target
            .get()
            .map(|current| {
                view! {
                    <div
                        class="fixed inset-0 z-50 bg-black/70 flex items-center justify-center p-4"
                        on:click=move |_| set_target(None)
                    >
                        <div
                            class="w-full max-w-2xl rounded-lg bg-slate-900 border border-slate-700 p-4"
                            on:click=move |ev| ev.stop_propagation()
                        >
                            <div class="flex items-center justify-between">
                                <div>
                                    <h3 class="text-white text-lg font-semibold">"Send PMT"</h3>
                                    <p class="text-slate-300 text-sm">
                                        {format!(
                                            "{} -> block {} ({}) / handler '{}'",
                                            current.source,
                                            current.block_id,
                                            current.block_name,
                                            current.handler,
                                        )}
                                    </p>
                                </div>
                                <button
                                    class="rounded bg-slate-700 hover:bg-slate-600 px-3 py-1 text-sm text-white"
                                    on:click=move |_| set_target(None)
                                    disabled=submitting
                                >
                                    "Close"
                                </button>
                            </div>
                            <div class="mt-3">
                                <PmtEditor
                                    on_submit=on_submit_pmt
                                    disabled=submitting()
                                    select_class="w-full rounded bg-slate-800 text-white px-2 py-2"
                                    input_class="w-full h-32 rounded bg-slate-800 text-white px-2 py-2 font-mono"
                                    error_class="text-red-400 text-sm"
                                    button_class="rounded bg-blue-600 hover:bg-blue-500 text-white px-3 py-2"
                                    button_text=if submitting() {
                                        "Sending...".to_string()
                                    } else {
                                        "Send".to_string()
                                    }
                                />
                            </div>
                            <div class="mt-2 text-red-400 text-sm">
                                {move || submit_error.get().unwrap_or_default()}
                            </div>
                        </div>
                    </div>
                }
            })}
    }
}

#[component]
/// Top-level chirpmunk GUI shell. Bootstraps a `RuntimeHandle` from the
/// browser origin and looks up FlowgraphId(0).
pub fn Gui() -> impl IntoView {
    let rt_url = window().location().origin().unwrap();
    let rt_handle = RuntimeHandle::from_url(rt_url);
    let fg_handle = LocalResource::new(move || {
        let rt_handle = rt_handle.clone();
        async move { rt_handle.get_flowgraph(FlowgraphId(0)).await.ok() }
    });

    view! {
        <div class="min-h-screen bg-slate-900">
            <header class="m-4 p-4 bg-slate-800 border border-slate-700 rounded-xl shadow-lg">
                <h1 class="text-2xl font-semibold text-white">"chirpmunk LoRa transceiver"</h1>
                <p class="text-sm text-slate-400 mt-1">
                    "Live spectrum, waterfall, and runtime flowgraph control via prophecy."
                </p>
            </header>
            {move || {
                if let Some(Some(handle)) = fg_handle.get() {
                    return view! { <Spectrum fg_handle=handle /> }.into_any();
                }
                view! { <div class="m-4 text-slate-400">"Connecting to chirpmunk-trx..."</div> }
                    .into_any()
            }}
        </div>
    }
}

pub fn frontend() {
    console_error_panic_hook::set_once();
    futuresdr::runtime::init();
    mount_to_body(|| view! { <Gui /> })
}
