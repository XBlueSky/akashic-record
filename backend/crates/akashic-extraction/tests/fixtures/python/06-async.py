import asyncio

async def fetch_one(url: str) -> str:
    await asyncio.sleep(0.1)
    return url

async def fetch_all(urls: list) -> list:
    return await asyncio.gather(*[fetch_one(u) for u in urls])
