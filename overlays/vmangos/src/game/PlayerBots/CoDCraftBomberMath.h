#ifndef CODCRAFT_BOMBER_MATH_H
#define CODCRAFT_BOMBER_MATH_H
namespace CoDCraftBomberMath {
// Convert before arithmetic: unsigned counters must not wrap negative offsets.
constexpr float Offset(unsigned index) { return -32.0f+8.0f*float(index); }
constexpr float Height(float start,unsigned ageMs) {
    return start-20.0f*(float(ageMs)/1000.0f)*(float(ageMs)/1000.0f);
}
}
#endif
