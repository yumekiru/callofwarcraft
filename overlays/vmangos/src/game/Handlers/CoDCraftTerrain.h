#pragma once
#include <algorithm>
#include <cmath>
#include <cstdint>

// ADT and VMAP are independent. Use the raw ADT height, not a nearby WMO floor.
template<class HeightAt>
bool CoDCraftTerrainSegmentClear(float x1, float y1, float z1,
    float x2, float y2, float z2, float invalidHeight, HeightAt heightAt)
{
    float dx = x2 - x1, dy = y2 - y1, dz = z2 - z1;
    auto steps = std::max<std::uint32_t>(1,
        std::uint32_t(std::ceil(std::sqrt(dx * dx + dy * dy) / 0.25f)));
    for (std::uint32_t i = 0; i <= steps; ++i)
    {
        float t = float(i) / float(steps);
        float z = z1 + dz * t;
        float ground = heightAt(x1 + dx * t, y1 + dy * t, z);
        if (!std::isfinite(ground) || ground <= invalidHeight || ground >= z - 0.02f)
            return false;
    }
    return true;
}
