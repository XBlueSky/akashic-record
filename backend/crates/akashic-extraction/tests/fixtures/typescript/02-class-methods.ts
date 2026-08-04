export class UserService {
  private users: User[] = [];

  public addUser(user: User): void {
    this.users.push(user);
  }

  async fetchUser(id: string): Promise<User | undefined> {
    return this.users.find(u => u.id === id);
  }
}

interface User {
  id: string;
  name: string;
}
