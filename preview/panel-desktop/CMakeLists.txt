# frameeyeosc-panel as a desktop window, for previewing it on a Linux PC without SteamVR.
# Builds the panel's own sources unchanged, except:
#   - vr_overlay.cpp / vk_texture.cpp (SteamVR and its Vulkan textures) are replaced by vr_overlay_desktop.cpp (SDL2)
#   - config.h's font paths point at this distribution's Noto Sans CJK (SteamOS keeps them elsewhere)
cmake_minimum_required(VERSION 3.16)
project(frameeyeosc-panel-preview LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

set(UPSTREAM "${CMAKE_CURRENT_SOURCE_DIR}/../../upstream/frameeyeosc" CACHE PATH "frameeyeosc source tree")
set(PANEL_FONT_REGULAR "/usr/share/fonts/google-noto-sans-cjk-fonts/NotoSansCJK-Regular.ttc" CACHE FILEPATH "Regular font")
set(PANEL_FONT_BOLD "/usr/share/fonts/google-noto-sans-cjk-fonts/NotoSansCJK-Bold.ttc" CACHE FILEPATH "Bold font")

# A copy of the panel's sources with the font paths replaced
set(SRC "${CMAKE_BINARY_DIR}/panel-src")
file(REMOVE_RECURSE "${SRC}")
file(COPY "${UPSTREAM}/panel/src/" DESTINATION "${SRC}")
file(READ "${SRC}/config.h" CONFIG_H)
string(REPLACE "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc" "${PANEL_FONT_REGULAR}" CONFIG_H "${CONFIG_H}")
string(REPLACE "/usr/share/fonts/noto-cjk/NotoSansCJK-Bold.ttc" "${PANEL_FONT_BOLD}" CONFIG_H "${CONFIG_H}")
file(WRITE "${SRC}/config.h" "${CONFIG_H}")

file(STRINGS "${UPSTREAM}/Cargo.toml" CARGO_VERSION_LINE REGEX "^version = \"[^\"]+\"" LIMIT_COUNT 1)
string(REGEX REPLACE "^version = \"([^\"]+)\".*" "\\1" FRAMEEYEOSC_VERSION "${CARGO_VERSION_LINE}")

find_package(PkgConfig REQUIRED)
find_package(Threads REQUIRED)
pkg_check_modules(CAIRO REQUIRED IMPORTED_TARGET cairo)
pkg_check_modules(FREETYPE REQUIRED IMPORTED_TARGET freetype2)
pkg_check_modules(SDL2 REQUIRED IMPORTED_TARGET sdl2)
# vk_texture.h (included by vr_overlay.h) needs the Vulkan headers only; nothing links Vulkan
find_path(VULKAN_INCLUDE vulkan/vulkan.h REQUIRED)

add_executable(frameeyeosc-panel-preview
  ${SRC}/main.cpp
  ${SRC}/autostart.cpp
  ${SRC}/command.cpp
  ${SRC}/config.cpp
  ${SRC}/draw.cpp
  ${SRC}/gaze_dots.cpp
  ${SRC}/gaze_fit.cpp
  ${SRC}/fit_text.cpp
  ${SRC}/host_entry.cpp
  ${SRC}/i18n.cpp
  ${SRC}/json.cpp
  ${SRC}/model.cpp
  ${SRC}/panel.cpp
  ${SRC}/sounds.cpp
  ${SRC}/status.cpp
  ${SRC}/target.cpp
  ${SRC}/theme.cpp
  ${UPSTREAM}/vendor/frame-updater/cpp/update_check.cpp
  vr_overlay_desktop.cpp
)
target_include_directories(frameeyeosc-panel-preview PRIVATE "${SRC}" "${UPSTREAM}/vendor/frame-updater/cpp" "${VULKAN_INCLUDE}")
target_compile_definitions(frameeyeosc-panel-preview PRIVATE FRAMEEYEOSC_VERSION="${FRAMEEYEOSC_VERSION}")
target_compile_options(frameeyeosc-panel-preview PRIVATE -Wall -Wextra)
target_link_libraries(frameeyeosc-panel-preview PRIVATE PkgConfig::CAIRO PkgConfig::FREETYPE PkgConfig::SDL2 Threads::Threads)
