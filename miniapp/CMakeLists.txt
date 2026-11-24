# The core module lives in the Rust crate under rust-lib/ (with its logic in logic/); the module builder links it into
# the plugin with the generated glue, so there are no C++ sources here.
cmake_minimum_required(VERSION 3.14)
project(CliffVeilModulePlugin LANGUAGES CXX)

if(DEFINED ENV{LOGOS_MODULE_BUILDER_ROOT})
    include($ENV{LOGOS_MODULE_BUILDER_ROOT}/cmake/LogosModule.cmake)
else()
    message(FATAL_ERROR "LOGOS_MODULE_BUILDER_ROOT is not set (build with nix)")
endif()

file(READ "${CMAKE_CURRENT_SOURCE_DIR}/metadata.json" METADATA_JSON)
string(JSON MODULE_NAME GET ${METADATA_JSON} name)
logos_module(NAME ${MODULE_NAME})
