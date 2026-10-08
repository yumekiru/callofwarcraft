#ifndef CODCRAFT_HELICOPTER_H
#define CODCRAFT_HELICOPTER_H
#include "Common.h"
class Player;
namespace CoDCraftHelicopter
{
    constexpr uint32 Spell = 24732;
    bool Call(Player* owner);
    void Update(uint32 diff);
}
#endif
