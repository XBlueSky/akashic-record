export const handler = (req: Request) => {
  return new Response("ok");
};

const factory = () => {
  return { create: () => ({}) };
};
