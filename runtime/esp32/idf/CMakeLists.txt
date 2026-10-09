# Optional ESP-IDF project. Configure only when IDF_PATH is available.
cmake_minimum_required(VERSION 3.16)

if(DEFINED ENV{IDF_PATH})
  include($ENV{IDF_PATH}/tools/cmake/project.cmake)
  project(mdc_esp32_runtime)
else()
  message(STATUS "IDF_PATH unset — skipping ESP-IDF project; use host runtime tests")
  project(mdc_esp32_idf_stub LANGUAGES C)
  add_library(mdc_idf_headers INTERFACE)
  target_include_directories(mdc_idf_headers INTERFACE port)
endif()
