// A desktop stand-in for frameeyeosc-panel's vr_overlay.cpp: the same panel in an ordinary window (SDL2)
// instead of on the SteamVR dashboard, so it can be tried on a PC without a headset. Everything else
// (drawing, buttons, settings, the eye fit, the sounds) is frameeyeosc-panel's own code, unchanged.
//
// While "the dashboard is open" the window shows the panel and the mouse works like the laser pointer.
// Space (or the button at the top) closes the dashboard: the window then shows what you would see in the
// headset, which is where the eye fit's dot and the debug gaze dots appear.

#include "vr_overlay.h"

#include "gaze_fit.h"

#include <SDL.h>
#include <cairo.h>

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

// vk_texture.cpp needs OpenVR, so it isn't built here; VrOverlay only holds these as (unused) members.
VulkanContext::~VulkanContext() {}
OverlayTexture::~OverlayTexture() {}

namespace {

constexpr int kBar = 46;              ///< the toolbar above the panel (logical px)
constexpr double kPxPerDegree = 12.0; ///< the headset view: 100 degrees across the 1200 px panel width
constexpr double kTargetWidthM = 0.3; ///< the eye fit's target is 0.3 m wide, 2 m ahead (see vr_overlay.cpp)
constexpr double kTargetDistanceM = 2.0;

struct Image {
    std::vector<uint8_t> rgba;  ///< non-premultiplied
    int size = 0;
    cairo_surface_t* surface = nullptr;

    void set(const uint8_t* data, int edge) {
        rgba.assign(data, data + static_cast<size_t>(edge) * edge * 4);
        size = edge;
        if (surface != nullptr) cairo_surface_destroy(surface);
        surface = nullptr;
    }
};

struct Dot {
    bool shown = false;
    double x = 0, y = 0, z = -1, width = 0.02;
    Image image;
};

struct Desktop {
    SDL_Window* window = nullptr;
    SDL_Renderer* renderer = nullptr;
    SDL_Texture* texture = nullptr;
    cairo_surface_t* frame = nullptr;  ///< the whole window: toolbar and view
    int width = 0;                     ///< logical size
    int height = 0;
    int panelHeight = 0;
    cairo_surface_t* panel = nullptr;
    bool dashboardOpen = true;
    bool pointerInPanel = false;
    bool buttonHover = false;
    bool dirty = true;
    Uint32 lastDraw = 0;
    bool targetShown = false;
    double targetYaw = 0, targetPitch = 0;
    Image target;
    Dot dots[2];
} gDesk;

/** Non-premultiplied RGBA into a cairo (premultiplied, native-endian ARGB) surface. */
cairo_surface_t* toSurface(const uint8_t* rgba, int width, int height) {
    cairo_surface_t* surface = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, width, height);
    cairo_surface_flush(surface);
    unsigned char* data = cairo_image_surface_get_data(surface);
    const int stride = cairo_image_surface_get_stride(surface);
    for (int y = 0; y < height; ++y) {
        uint32_t* row = reinterpret_cast<uint32_t*>(data + static_cast<size_t>(y) * stride);
        const uint8_t* in = rgba + static_cast<size_t>(y) * width * 4;
        for (int x = 0; x < width; ++x, in += 4) {
            const uint32_t a = in[3];
            row[x] = (a << 24) | ((in[0] * a / 255) << 16) | ((in[1] * a / 255) << 8) | (in[2] * a / 255);
        }
    }
    cairo_surface_mark_dirty(surface);
    return surface;
}

cairo_surface_t* surfaceOf(Image& image) {
    if (image.surface == nullptr && image.size > 0) image.surface = toSurface(image.rgba.data(), image.size, image.size);
    return image.surface;
}

void rgb(cairo_t* cr, uint32_t hex, double alpha = 1.0) {
    cairo_set_source_rgba(cr, ((hex >> 16) & 0xff) / 255.0, ((hex >> 8) & 0xff) / 255.0, (hex & 0xff) / 255.0, alpha);
}

void text(cairo_t* cr, double x, double y, const char* utf8, double size, uint32_t color, bool bold = false,
          bool centered = false) {
    cairo_select_font_face(cr, "Noto Sans", CAIRO_FONT_SLANT_NORMAL, bold ? CAIRO_FONT_WEIGHT_BOLD : CAIRO_FONT_WEIGHT_NORMAL);
    cairo_set_font_size(cr, size);
    if (centered) {
        cairo_text_extents_t extents;
        cairo_text_extents(cr, utf8, &extents);
        x -= extents.width / 2 + extents.x_bearing;
    }
    rgb(cr, color);
    cairo_move_to(cr, x, y);
    cairo_show_text(cr, utf8);
}

/** The toolbar button's rectangle (logical px). */
void buttonRect(double& x, double& y, double& w, double& h) {
    w = 250;
    h = 32;
    x = gDesk.width - w - 10;
    y = (kBar - h) / 2;
}

bool inButton(double px, double py) {
    double x, y, w, h;
    buttonRect(x, y, w, h);
    return px >= x && px <= x + w && py >= y && py <= y + h;
}

void roundedRect(cairo_t* cr, double x, double y, double w, double h, double r) {
    cairo_new_sub_path(cr);
    cairo_arc(cr, x + w - r, y + r, r, -M_PI / 2, 0);
    cairo_arc(cr, x + w - r, y + h - r, r, 0, M_PI / 2);
    cairo_arc(cr, x + r, y + h - r, r, M_PI / 2, M_PI);
    cairo_arc(cr, x + r, y + r, r, M_PI, 3 * M_PI / 2);
    cairo_close_path(cr);
}

/** Draw an image centered at a direction in front of you, `degrees` wide. */
void drawInView(cairo_t* cr, cairo_surface_t* image, int size, double yawDeg, double pitchDeg, double degrees) {
    if (image == nullptr || size <= 0) return;
    const double cx = gDesk.width / 2.0 + yawDeg * kPxPerDegree;
    const double cy = kBar + gDesk.panelHeight / 2.0 - pitchDeg * kPxPerDegree;
    const double px = degrees * kPxPerDegree;
    cairo_save(cr);
    cairo_translate(cr, cx - px / 2, cy - px / 2);
    cairo_scale(cr, px / size, px / size);
    cairo_set_source_surface(cr, image, 0, 0);
    cairo_pattern_set_filter(cairo_get_source(cr), CAIRO_FILTER_GOOD);
    cairo_paint(cr);
    cairo_restore(cr);
}

void drawHeadsetView(cairo_t* cr) {
    const double top = kBar;
    const double h = gDesk.panelHeight;
    const double w = gDesk.width;
    cairo_pattern_t* sky = cairo_pattern_create_radial(w / 2, top + h / 2, 40, w / 2, top + h / 2, w * 0.75);
    cairo_pattern_add_color_stop_rgb(sky, 0, 0.09, 0.11, 0.15);
    cairo_pattern_add_color_stop_rgb(sky, 1, 0.02, 0.03, 0.05);
    cairo_rectangle(cr, 0, top, w, h);
    cairo_set_source(cr, sky);
    cairo_fill(cr);
    cairo_pattern_destroy(sky);
    // A faint grid every 10 degrees, brighter through the middle
    cairo_set_line_width(cr, 1);
    for (int degree = -50; degree <= 50; degree += 10) {
        rgb(cr, degree == 0 ? 0x3a4350 : 0x1c222b);
        const double x = w / 2 + degree * kPxPerDegree + 0.5;
        cairo_move_to(cr, x, top);
        cairo_line_to(cr, x, top + h);
        cairo_stroke(cr);
    }
    for (int degree = -30; degree <= 30; degree += 10) {
        rgb(cr, degree == 0 ? 0x3a4350 : 0x1c222b);
        const double y = top + h / 2 - degree * kPxPerDegree + 0.5;
        cairo_move_to(cr, 0, y);
        cairo_line_to(cr, w, y);
        cairo_stroke(cr);
    }
    text(cr, w / 2 + 6, top + 18, "straight ahead", 12, 0x5d6672);
    text(cr, 8, top + h / 2 - 6, "eye level", 12, 0x5d6672);

    if (gDesk.targetShown) {
        const double degrees = 2 * std::atan(kTargetWidthM / 2 / kTargetDistanceM) * 180 / M_PI;
        drawInView(cr, surfaceOf(gDesk.target), gDesk.target.size, gDesk.targetYaw, gDesk.targetPitch, degrees);
    }
    bool anyDot = false;
    for (Dot& dot : gDesk.dots) {
        if (!dot.shown) continue;
        anyDot = true;
        const double distance = std::sqrt(dot.x * dot.x + dot.y * dot.y + dot.z * dot.z);
        const double yaw = std::atan2(dot.x, -dot.z) * 180 / M_PI;
        const double pitch = std::atan2(dot.y, std::sqrt(dot.x * dot.x + dot.z * dot.z)) * 180 / M_PI;
        const double degrees = 2 * std::atan(dot.width / 2 / std::max(distance, 0.05)) * 180 / M_PI;
        drawInView(cr, surfaceOf(dot.image), dot.image.size, yaw, pitch, std::max(degrees, 1.2));
    }

    const double bottom = top + h;
    text(cr, w / 2, bottom - 44, "The dashboard is closed: this is what you would see in the headset.", 16, 0xc9d1d9, true, true);
    if (gDesk.targetShown) {
        text(cr, w / 2, bottom - 20, "The eye fit is running. The pretend eyes follow the dot, like you would with yours.", 14, 0x9198a1,
             false, true);
    } else if (anyDot) {
        text(cr, w / 2, bottom - 20, "The dots show where the eyes being sent to VRChat are looking (Show gaze dots is on).", 14,
             0x9198a1, false, true);
    } else {
        text(cr, w / 2, bottom - 20,
             "Press Space to open the dashboard again. (Eye fit tab: press Fit my eyes, then close the dashboard.)", 14,
             0x9198a1, false, true);
    }
}

void redraw() {
    if (gDesk.renderer == nullptr) return;
    cairo_t* cr = cairo_create(gDesk.frame);
    // Toolbar
    rgb(cr, 0x0d1117);
    cairo_rectangle(cr, 0, 0, gDesk.width, kBar);
    cairo_fill(cr);
    rgb(cr, 0x30363d);
    cairo_rectangle(cr, 0, kBar - 1, gDesk.width, 1);
    cairo_fill(cr);
    text(cr, 14, 20, "Eye panel preview", 15, 0xe6edf3, true);
    text(cr, 14, 38, "A pretend Steam Frame on this PC. Nothing is sent to VRChat.", 12.5, 0x9198a1);
    double bx, by, bw, bh;
    buttonRect(bx, by, bw, bh);
    roundedRect(cr, bx, by, bw, bh, 7);
    rgb(cr, gDesk.buttonHover ? 0xc45fe0 : 0xe27dfd);
    cairo_fill(cr);
    text(cr, bx + bw / 2, by + 21, gDesk.dashboardOpen ? "Close the dashboard  (Space)" : "Open the dashboard  (Space)", 14,
         0x0d1117, true, true);
    // The view
    if (gDesk.dashboardOpen) {
        rgb(cr, 0x0d1117);
        cairo_rectangle(cr, 0, kBar, gDesk.width, gDesk.panelHeight);
        cairo_fill(cr);
        if (gDesk.panel != nullptr) {
            cairo_set_source_surface(cr, gDesk.panel, 0, kBar);
            cairo_paint(cr);
        }
    } else {
        drawHeadsetView(cr);
    }
    cairo_destroy(cr);
    cairo_surface_flush(gDesk.frame);
    SDL_UpdateTexture(gDesk.texture, nullptr, cairo_image_surface_get_data(gDesk.frame),
                      cairo_image_surface_get_stride(gDesk.frame));
    SDL_SetRenderDrawColor(gDesk.renderer, 0x0d, 0x11, 0x17, 0xff);
    SDL_RenderClear(gDesk.renderer);
    SDL_RenderCopy(gDesk.renderer, gDesk.texture, nullptr, nullptr);
    SDL_RenderPresent(gDesk.renderer);
    gDesk.dirty = false;
    gDesk.lastDraw = SDL_GetTicks();
}

/**
 * For testing and for taking pictures: PREVIEW_SCRIPT runs simple steps separated by ";":
 *   wait SECONDS | click X,Y (panel px) | key space | shot FILE.png | quit
 */
struct Script {
    std::vector<std::string> steps;
    size_t next = 0;
    Uint32 resumeAt = 0;
    bool loaded = false;
} gScript;

void setDashboard(bool open, VrEvents& events);

void runScript(VrEvents& events) {
    if (!gScript.loaded) {
        gScript.loaded = true;
        if (const char* script = std::getenv("PREVIEW_SCRIPT")) {
            std::string all = script;
            size_t start = 0;
            while (start <= all.size()) {
                const size_t end = all.find(';', start);
                std::string step = all.substr(start, end == std::string::npos ? std::string::npos : end - start);
                const size_t first = step.find_first_not_of(" \t");
                if (first != std::string::npos) gScript.steps.push_back(step.substr(first));
                if (end == std::string::npos) break;
                start = end + 1;
            }
        }
    }
    while (gScript.next < gScript.steps.size() && SDL_GetTicks() >= gScript.resumeAt) {
        const std::string step = gScript.steps[gScript.next++];
        double x = 0, y = 0, seconds = 0;
        char path[512] = {};
        if (std::sscanf(step.c_str(), "wait %lf", &seconds) == 1) {
            gScript.resumeAt = SDL_GetTicks() + static_cast<Uint32>(seconds * 1000);
        } else if (std::sscanf(step.c_str(), "click %lf,%lf", &x, &y) == 2) {
            events.pointer.push_back({PointerInput::Type::Move, x, y});
            events.pointer.push_back({PointerInput::Type::Down, x, y});
            events.pointer.push_back({PointerInput::Type::Up, x, y});
            gScript.resumeAt = SDL_GetTicks() + 150;
        } else if (step == "key space") {
            setDashboard(!gDesk.dashboardOpen, events);
        } else if (std::sscanf(step.c_str(), "shot %511s", path) == 1) {
            redraw();
            const cairo_status_t result = cairo_surface_write_to_png(gDesk.frame, path);
            std::fprintf(stderr, "[preview] %s %s\n", result == CAIRO_STATUS_SUCCESS ? "saved" : "could not save", path);
        } else if (step == "quit") {
            events.closeRequested = true;
        } else {
            std::fprintf(stderr, "[preview] unknown script step: %s\n", step.c_str());
        }
    }
}

void setDashboard(bool open, VrEvents& events) {
    if (gDesk.dashboardOpen == open) return;
    gDesk.dashboardOpen = open;
    if (!open && gDesk.pointerInPanel) {
        events.pointer.push_back({PointerInput::Type::Leave, 0, 0});
        gDesk.pointerInPanel = false;
    }
    std::fprintf(stderr, "[preview] dashboard %s\n", open ? "opened" : "closed");
    gDesk.dirty = true;
}

}  // namespace

VrOverlay::VrOverlay() = default;

VrOverlay::~VrOverlay() {
    shutdown();
}

VrOverlay::ConnectResult VrOverlay::connect(int width, int height, std::string& message) {
    SDL_SetHint(SDL_HINT_RENDER_SCALE_QUALITY, "linear");
    SDL_SetHint(SDL_HINT_VIDEO_ALLOW_SCREENSAVER, "1");
    if (SDL_Init(SDL_INIT_VIDEO | SDL_INIT_EVENTS) != 0) {
        message = std::string("SDL: ") + SDL_GetError();
        return ConnectResult::Error;
    }
    gDesk.width = width;
    gDesk.panelHeight = height;
    gDesk.height = height + kBar;
    gDesk.window = SDL_CreateWindow("Eye panel preview (frameeyeosc)", SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED,
                                    gDesk.width, gDesk.height, SDL_WINDOW_RESIZABLE | SDL_WINDOW_ALLOW_HIGHDPI);
    if (gDesk.window == nullptr) {
        message = std::string("SDL window: ") + SDL_GetError();
        SDL_Quit();
        return ConnectResult::Error;
    }
    gDesk.renderer = SDL_CreateRenderer(gDesk.window, -1, SDL_RENDERER_ACCELERATED | SDL_RENDERER_PRESENTVSYNC);
    if (gDesk.renderer == nullptr) gDesk.renderer = SDL_CreateRenderer(gDesk.window, -1, SDL_RENDERER_SOFTWARE);
    if (gDesk.renderer == nullptr) {
        message = std::string("SDL renderer: ") + SDL_GetError();
        return ConnectResult::Error;
    }
    SDL_RenderSetLogicalSize(gDesk.renderer, gDesk.width, gDesk.height);
    gDesk.texture = SDL_CreateTexture(gDesk.renderer, SDL_PIXELFORMAT_ARGB8888, SDL_TEXTUREACCESS_STREAMING, gDesk.width,
                                      gDesk.height);
    gDesk.frame = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, gDesk.width, gDesk.height);
    connected_ = true;
    panelHeight_ = height;
    std::fprintf(stderr, "[preview] window %dx%d (the panel plus a %d px toolbar)\n", gDesk.width, gDesk.height, kBar);
    redraw();
    return ConnectResult::Ok;
}

void VrOverlay::shutdown() {
    if (!connected_) return;
    connected_ = false;
    for (Image* image : {&gDesk.target, &gDesk.dots[0].image, &gDesk.dots[1].image}) {
        if (image->surface != nullptr) cairo_surface_destroy(image->surface);
        image->surface = nullptr;
    }
    if (gDesk.panel != nullptr) cairo_surface_destroy(gDesk.panel);
    if (gDesk.frame != nullptr) cairo_surface_destroy(gDesk.frame);
    gDesk.panel = gDesk.frame = nullptr;
    if (gDesk.texture != nullptr) SDL_DestroyTexture(gDesk.texture);
    if (gDesk.renderer != nullptr) SDL_DestroyRenderer(gDesk.renderer);
    if (gDesk.window != nullptr) SDL_DestroyWindow(gDesk.window);
    gDesk.texture = nullptr;
    gDesk.renderer = nullptr;
    gDesk.window = nullptr;
    SDL_Quit();
    std::fprintf(stderr, "[preview] window closed\n");
}

VrEvents VrOverlay::pollEvents() {
    VrEvents events;
    SDL_Event event;
    while (SDL_PollEvent(&event)) {
        switch (event.type) {
            case SDL_QUIT:
                // Like the dashboard icon's "close": the panel quits
                events.closeRequested = true;
                break;
            case SDL_KEYDOWN:
                if (event.key.repeat == 0 && (event.key.keysym.sym == SDLK_SPACE || event.key.keysym.sym == SDLK_TAB ||
                                              event.key.keysym.sym == SDLK_ESCAPE)) {
                    setDashboard(event.key.keysym.sym == SDLK_ESCAPE ? false : !gDesk.dashboardOpen, events);
                }
                break;
            case SDL_MOUSEMOTION: {
                const double x = event.motion.x, y = event.motion.y;
                const bool hover = inButton(x, y);
                if (hover != gDesk.buttonHover) {
                    gDesk.buttonHover = hover;
                    gDesk.dirty = true;
                }
                if (gDesk.dashboardOpen && y >= kBar) {
                    events.pointer.push_back({PointerInput::Type::Move, x, y - kBar});
                    gDesk.pointerInPanel = true;
                } else if (gDesk.pointerInPanel) {
                    events.pointer.push_back({PointerInput::Type::Leave, 0, 0});
                    gDesk.pointerInPanel = false;
                }
                break;
            }
            case SDL_MOUSEBUTTONDOWN:
                if (event.button.button != SDL_BUTTON_LEFT) break;
                if (inButton(event.button.x, event.button.y)) {
                    setDashboard(!gDesk.dashboardOpen, events);
                } else if (gDesk.dashboardOpen && event.button.y >= kBar) {
                    events.pointer.push_back({PointerInput::Type::Down, double(event.button.x), double(event.button.y - kBar)});
                } else if (!gDesk.dashboardOpen && event.button.y >= kBar) {
                    // Clicking into the headset view opens the dashboard again, like the system button
                    setDashboard(true, events);
                }
                break;
            case SDL_MOUSEBUTTONUP:
                if (event.button.button == SDL_BUTTON_LEFT && gDesk.dashboardOpen && event.button.y >= kBar) {
                    events.pointer.push_back({PointerInput::Type::Up, double(event.button.x), double(event.button.y - kBar)});
                }
                break;
            case SDL_WINDOWEVENT:
                if (event.window.event == SDL_WINDOWEVENT_LEAVE && gDesk.pointerInPanel) {
                    events.pointer.push_back({PointerInput::Type::Leave, 0, 0});
                    gDesk.pointerInPanel = false;
                }
                gDesk.dirty = true;
                break;
            default: break;
        }
    }
    runScript(events);
    // The headset view moves (the fit's dot, the gaze dots) without the panel changing, so it is redrawn
    // regularly; the panel view only when something changed.
    const Uint32 now = SDL_GetTicks();
    if (gDesk.dirty || (!gDesk.dashboardOpen && now - gDesk.lastDraw >= 11) || now - gDesk.lastDraw >= 500) redraw();
    return events;
}

bool VrOverlay::steamVrAlive() const {
    return true;
}

bool VrOverlay::panelVisible() const {
    return gDesk.dashboardOpen;
}

void VrOverlay::showPanel() {
    VrEvents ignored;
    setDashboard(true, ignored);
    if (gDesk.window != nullptr) SDL_RaiseWindow(gDesk.window);
}

bool VrOverlay::dashboardVisible() const {
    return gDesk.dashboardOpen;
}

double VrOverlay::userIpdMeters() const {
    return gaze_fit::kDefaultIpdM;
}

bool VrOverlay::showTarget(double yawDeg, double pitchDeg, const uint8_t* rgba, int size) {
    if (rgba != nullptr) gDesk.target.set(rgba, size);
    if (!gDesk.targetShown || yawDeg != gDesk.targetYaw || pitchDeg != gDesk.targetPitch || rgba != nullptr) gDesk.dirty = true;
    gDesk.targetShown = true;
    gDesk.targetYaw = yawDeg;
    gDesk.targetPitch = pitchDeg;
    return true;
}

bool VrOverlay::waitFrameSync(uint32_t /*timeoutMs*/) {
    return false;
}

void VrOverlay::hideTarget() {
    if (gDesk.targetShown) gDesk.dirty = true;
    gDesk.targetShown = false;
}

bool VrOverlay::showDot(int index, double x, double y, double z, double /*yawDeg*/, double /*pitchDeg*/, const uint8_t* rgba,
                        int size, bool newImage, double widthM) {
    if (index < 0 || index > 1) return false;
    Dot& dot = gDesk.dots[index];
    if (newImage || dot.image.size == 0) dot.image.set(rgba, size);
    dot.shown = true;
    dot.x = x;
    dot.y = y;
    dot.z = z;
    dot.width = widthM;
    return true;
}

void VrOverlay::hideDot(int index) {
    if (index >= 0 && index <= 1) gDesk.dots[index].shown = false;
}

bool VrOverlay::submitThumbnail(const uint8_t* rgba, int size) {
    if (gDesk.window == nullptr) return false;
    // The dashboard thumbnail (the eye) becomes the window icon
    SDL_Surface* icon = SDL_CreateRGBSurfaceWithFormatFrom(const_cast<uint8_t*>(rgba), size, size, 32, size * 4,
                                                           SDL_PIXELFORMAT_RGBA32);
    if (icon == nullptr) return false;
    SDL_SetWindowIcon(gDesk.window, icon);
    SDL_FreeSurface(icon);
    return true;
}

bool VrOverlay::submitPanel(const uint8_t* rgba) {
    if (gDesk.panel != nullptr) cairo_surface_destroy(gDesk.panel);
    gDesk.panel = toSurface(rgba, gDesk.width, gDesk.panelHeight);
    gDesk.dirty = true;
    return true;
}

void VrOverlay::logOverlayState(const char* when) const {
    std::fprintf(stderr, "[preview] panel shown in a desktop window (%s)\n", when);
}

int VrOverlay::probe() {
    std::printf("--probe needs SteamVR; this is the desktop preview build\n");
    return 1;
}

int VrOverlay::switchAway(double /*seconds*/) {
    std::printf("--probe-switch-away needs SteamVR; this is the desktop preview build\n");
    return 1;
}
