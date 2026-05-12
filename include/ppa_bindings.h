// Headers fed to esp-idf-sys's bindgen pass when `buoyant-esp32p4`'s
// `accel-ppa` feature is active. esp-idf-sys aggregates metadata from
// the full dependency graph, so the downstream binary crate (e.g.
// rlvgl-starter's firmware-tab5) inherits these bindings automatically
// once it enables the feature.
//
// The symbols defined here live under `esp_idf_sys::ppa::*` once
// generated; see Cargo.toml's `extra_components` entry for the wiring.
#pragma once

#include "driver/ppa.h"
#include "esp_heap_caps.h"
#include "esp_cache.h"
#include "soc/soc_caps.h"
