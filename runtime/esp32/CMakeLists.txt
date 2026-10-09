cmake_minimum_required(VERSION 3.16)
project(mdc_runtime_parser LANGUAGES CXX)
set(CMAKE_CXX_STANDARD 17)
add_library(mdc_runtime STATIC
  src/parser.cpp
  src/frame_store.cpp
  src/sim_display.cpp
  src/dispatcher.cpp
  src/cbor_control.cpp)
target_include_directories(mdc_runtime PUBLIC include)
add_executable(mdc_runtime_tests tests/parser_test.cpp)
target_link_libraries(mdc_runtime_tests PRIVATE mdc_runtime)
add_executable(cbor_control_test tests/cbor_control_test.cpp)
target_link_libraries(cbor_control_test PRIVATE mdc_runtime)
enable_testing()
add_test(NAME mdc_runtime_tests COMMAND mdc_runtime_tests)
add_test(NAME cbor_control_test COMMAND cbor_control_test)
