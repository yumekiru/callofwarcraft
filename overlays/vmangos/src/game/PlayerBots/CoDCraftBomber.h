#ifndef CODCRAFT_BOMBER_H
#define CODCRAFT_BOMBER_H
#include "Common.h"
class Player;
namespace CoDCraftBomber {
    constexpr uint32 Spell=24734;
    bool Call(Player*,float x,float y,float z,float heading);
    void Update(uint32 diff);
}
#endif
