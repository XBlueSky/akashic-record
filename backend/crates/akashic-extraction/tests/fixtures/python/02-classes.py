class UserService:
    def __init__(self, db):
        self.db = db

    def add_user(self, user):
        self.db.insert(user)

    @staticmethod
    def validate(user):
        return user.name is not None

    @classmethod
    def from_dict(cls, data):
        return cls(data["db"])
