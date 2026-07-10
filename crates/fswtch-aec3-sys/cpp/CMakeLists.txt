cmake_minimum_required(VERSION 3.16)
project(fswtch_aec3 LANGUAGES CXX C)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# Scalar build: AEC3 AVX2/NEON SIMD paths are disabled; only portable C++ is compiled.
# (CMake flags to disable SIMD are wired up as the AEC3 closure is vendored in later phases.)

# Phase 0: trivial smoke object to prove the cmake -> static lib -> bindgen -> Rust pipeline.
add_library(fswtch_aec3 STATIC
    wrapper/aec3_c_api.cc
)

target_include_directories(fswtch_aec3 PUBLIC ${CMAKE_CURRENT_SOURCE_DIR})

# The `cmake` crate runs `cmake --build . --target install` with CMAKE_INSTALL_PREFIX set to a
# path it returns as the link-search dir. Install the archive to that prefix root so the static
# lib lands at `<dst>/libfswtch_aec3.a` and `cargo:rustc-link-search=native=<dst>` finds it.
install(TARGETS fswtch_aec3 ARCHIVE DESTINATION .)
