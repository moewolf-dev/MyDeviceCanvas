cmake_minimum_required(VERSION 3.16)
project(mdc_linux_runtime LANGUAGES CXX)
set(CMAKE_CXX_STANDARD 17)

set(MDC_ESP32_INCLUDE "${CMAKE_CURRENT_SOURCE_DIR}/../esp32/include")
set(MDC_ESP32_SRC "${CMAKE_CURRENT_SOURCE_DIR}/../esp32/src")

add_library(mdc_linux_runtime STATIC
  src/linux_runtime.cpp
  ${MDC_ESP32_SRC}/sim_display.cpp
  ${MDC_ESP32_SRC}/frame_store.cpp
  ${MDC_ESP32_SRC}/parser.cpp
  ${MDC_ESP32_SRC}/dispatcher.cpp
  ${MDC_ESP32_SRC}/cbor_control.cpp)
target_include_directories(mdc_linux_runtime PUBLIC
  include
  ${MDC_ESP32_INCLUDE})

add_executable(linux_runtime_test tests/linux_runtime_test.cpp)
target_link_libraries(linux_runtime_test PRIVATE mdc_linux_runtime)

add_executable(mdc_linux_peer main/mdc_linux_peer.cpp)
target_link_libraries(mdc_linux_peer PRIVATE mdc_linux_runtime)

add_executable(cbor_control_test ${CMAKE_CURRENT_SOURCE_DIR}/../esp32/tests/cbor_control_test.cpp)
target_link_libraries(cbor_control_test PRIVATE mdc_linux_runtime)

enable_testing()
add_test(NAME linux_runtime_test COMMAND linux_runtime_test)
add_test(NAME cbor_control_test COMMAND cbor_control_test)
